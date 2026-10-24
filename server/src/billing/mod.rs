//! Subscriptions, for the managed tier only.
//!
//! Everything here is inert when Stripe is not configured, which is
//! the whole point: a self-hosted Relay must not acquire a payment
//! provider because the managed one has one. `Billing::Unmetered` is
//! the default and entitles everything.
//!
//! The rule that shapes the rest: **a billing problem must never lock
//! somebody out of their own data.** Stripe being unreachable, a card
//! expiring, a webhook arriving late — none of those may stop a person
//! reading or writing the workspace they already have. The only thing
//! an entitlement gates is *growth*: adding another seat.

pub mod stripe;
pub mod webhook;

use serde::{Deserialize, Serialize};

/// What a workspace is entitled to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    /// How many members are paid for. `None` means no limit.
    pub seats: Option<u32>,
    /// Whether the subscription is in good standing.
    pub active: bool,
    /// What to tell the person, when there is something to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Assistant turns included per period. `None` means the
    /// assistant is not included and the person uses their own key —
    /// which everybody can, free and unmetered, paying or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_turns: Option<u32>,
    /// Whether this workspace may sync at all.
    ///
    /// False in exactly one case: a deployment that charges, and a
    /// workspace nobody has ever paid for. A subscription that has
    /// lapsed, failed or been cancelled keeps syncing — that
    /// distinction is the whole point. Refusing to start storing
    /// somebody's work until they pay is a price; refusing to hand
    /// back work already stored because a card failed is a hostage,
    /// and a tool that holds your API requests must not do the
    /// second.
    #[serde(default = "yes")]
    pub syncs: bool,
}

/// `serde` default for [`Entitlement::syncs`]: anything that does not
/// say otherwise syncs.
fn yes() -> bool {
    true
}

impl Entitlement {
    /// No limits. What self-hosting always gets.
    pub fn unmetered() -> Self {
        Self {
            seats: None,
            active: true,
            notice: None,
            // Self-hosting charges for nothing, and nobody runs a
            // model on Relay's key.
            assistant_turns: None,
            syncs: true,
        }
    }

    /// A workspace on a charging deployment that nobody has paid for.
    ///
    /// Not the same as a lapsed subscription, which keeps syncing.
    /// Nothing of theirs is on the server yet, so nothing is being
    /// withheld — this is a price, asked before the work starts.
    pub fn unsubscribed() -> Self {
        Self {
            seats: Some(0),
            active: false,
            notice: Some(
                "This workspace is not subscribed. Everything in it stays on this \
                 machine and works exactly as it does now; subscribing is what puts \
                 it on the server."
                    .into(),
            ),
            assistant_turns: None,
            syncs: false,
        }
    }

    /// Whether another member may be added.
    pub fn allows(&self, current_members: u32) -> bool {
        match self.seats {
            None => true,
            Some(seats) => current_members < seats,
        }
    }
}

/// How a subscription stands, as Stripe describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// Paid and current.
    Active,
    /// In a trial.
    Trialing,
    /// A payment failed and Stripe is retrying.
    PastDue,
    /// Cancelled, or never completed.
    Canceled,
    /// Anything Stripe sends that this build does not know.
    Unknown,
}

impl SubscriptionStatus {
    /// Reads Stripe's spelling.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => Self::Active,
            "trialing" => Self::Trialing,
            "past_due" | "unpaid" => Self::PastDue,
            "canceled" | "incomplete_expired" => Self::Canceled,
            _ => Self::Unknown,
        }
    }

    /// Whether this status still entitles the workspace.
    ///
    /// `PastDue` deliberately does: Stripe retries a failed payment for
    /// days, and locking a team out of their work on the first failed
    /// card — often a bank's fraud check, not a real problem — would be
    /// a worse mistake than carrying them for a week. `Canceled` is a
    /// decision somebody made, so it does not.
    pub fn entitles(self) -> bool {
        matches!(self, Self::Active | Self::Trialing | Self::PastDue)
    }

    /// What to show the person about it.
    pub fn notice(self) -> Option<String> {
        match self {
            Self::PastDue => Some(
                "The last payment for this workspace did not go through. Everything \
                 still works; update the card to keep it that way."
                    .into(),
            ),
            Self::Canceled => Some(
                "This workspace's subscription has ended. Your data is untouched and \
                 still syncs; you cannot add anybody new until it is renewed."
                    .into(),
            ),
            Self::Unknown => Some(
                "This workspace's subscription is in a state Relay does not recognise. \
                 Nothing is blocked; please get in touch."
                    .into(),
            ),
            _ => None,
        }
    }
}

/// What one person has spent on the hosted assistant this period.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantUsage {
    /// Turns taken. This is what the cap counts, and what a person is
    /// shown, because "847 of your 1,000 requests" is a sentence
    /// somebody can act on and a token count is not.
    pub turns: u32,
    /// Tokens, which are the actual cost. Kept so the price can be
    /// checked against reality rather than against the estimate.
    pub input_tokens: u64,
    /// Tokens generated.
    pub output_tokens: u64,
    /// Input tokens served from the prompt cache, billed at a tenth.
    pub cached_tokens: u64,
}

/// What a person is allowed to do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Allowance {
    /// Turns included this period. `None` means the hosted assistant
    /// is not part of this person's plan at all.
    pub included: Option<u32>,
    /// Turns already taken.
    pub used: u32,
    /// Whether another turn may start.
    pub allowed: bool,
    /// What to tell them, when there is something to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl Allowance {
    /// Decides whether another turn may start.
    ///
    /// A pure function for the same reason the conflict decisions are
    /// (`server/src/store/logic.rs`): the answer must not depend on
    /// which database is underneath, or the same subscription would
    /// be cut off on one and not the other.
    ///
    /// Not being included is not a failure. Everybody can use the
    /// assistant with their own key, free and unmetered, so the
    /// notice says that rather than suggesting something is wrong.
    pub fn decide(included: Option<u32>, usage: AssistantUsage) -> Self {
        let used = usage.turns;

        match included {
            None => Self {
                included: None,
                used,
                allowed: false,
                notice: Some(
                    "This plan does not include the assistant. Add your own API key \
                     in Settings and it works, with no limit."
                        .into(),
                ),
            },
            Some(limit) if used >= limit => Self {
                included,
                used,
                allowed: false,
                notice: Some(format!(
                    "You have used all {limit} assistant requests included this month. \
                     They reset on the first. Add your own API key in Settings to carry \
                     on now, with no limit."
                )),
            },
            Some(limit) => Self {
                included,
                used,
                allowed: true,
                // Said once, near the end, rather than counted down
                // all month.
                notice: (used * 10 >= limit * 9)
                    .then(|| format!("{} of {limit} assistant requests used this month.", used)),
            },
        }
    }
}

/// The first millisecond of the calendar month `now` falls in, UTC.
///
/// Calendar months rather than the Stripe billing period: usage is
/// recorded per person, a person can belong to workspaces on
/// different subscriptions, and there is no one period to use. It is
/// also what anybody reading "this month" already assumes.
pub fn period_start(now_millis: i64) -> i64 {
    let days = now_millis.div_euclid(86_400_000);
    let (year, month, _) = civil_from_days(days);
    days_from_civil(year, month, 1) * 86_400_000
}

/// Days since the epoch to a civil date. Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The inverse.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// What a subscription includes.
///
/// Read from the Stripe price the subscription is actually on, rather
/// than stored as a name of its own, so the two cannot disagree. A
/// deployment that has not configured an assistant price has only one
/// plan, which is the self-hosting case and most of the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Plan {
    /// Sync. The assistant still works — with the person's own key,
    /// as it does for everybody, paying or not.
    Sync,
    /// Sync, and an allowance of assistant usage on Relay's key.
    SyncAndAssistant,
}

impl Plan {
    /// Which plan a price id means.
    ///
    /// Anything that is not the configured assistant price is the sync
    /// plan, including a price this build has never heard of. Failing
    /// that way round matters: a mismatch must not hand out model
    /// usage nobody is paying for.
    pub fn for_price(price_id: Option<&str>, assistant_prices: &[&str]) -> Self {
        match price_id {
            Some(price) if assistant_prices.contains(&price) => Self::SyncAndAssistant,
            _ => Self::Sync,
        }
    }

    /// Whether the assistant runs on Relay's key under this plan.
    pub fn includes_assistant(self) -> bool {
        matches!(self, Self::SyncAndAssistant)
    }
}

/// A workspace's subscription, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Which workspace.
    pub workspace_id: String,
    /// Stripe's customer id.
    pub customer_id: String,
    /// Stripe's subscription id, once there is one.
    pub subscription_id: Option<String>,
    /// How it stands.
    pub status: SubscriptionStatus,
    /// Seats paid for.
    pub seats: u32,
    /// When the paid period ends, in epoch milliseconds.
    pub current_period_end: Option<i64>,
    /// The Stripe price this subscription is on, which is what says
    /// whether the assistant is included. `None` on a subscription
    /// recorded before prices were distinguished.
    pub price_id: Option<String>,
}

impl Subscription {
    /// What this subscription entitles, as far as the store can say.
    ///
    /// Seats only. Whether the assistant is included depends on which
    /// price this is, and what that price means is a deployment's
    /// configuration rather than a property of the row — so the
    /// handler fills `assistant_turns` in, where the configuration is.
    pub fn entitlement(&self) -> Entitlement {
        Entitlement {
            seats: Some(self.seats),
            active: self.status.entitles(),
            notice: self.status.notice(),
            assistant_turns: None,
            // Having paid once is enough. A failed card stops growth,
            // never access to work already here.
            syncs: true,
        }
    }

    /// Which plan this is, given what the deployment charges for.
    pub fn plan(&self, assistant_prices: &[&str]) -> Plan {
        Plan::for_price(self.price_id.as_deref(), assistant_prices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn used(turns: u32) -> AssistantUsage {
        AssistantUsage {
            turns,
            ..Default::default()
        }
    }

    #[test]
    fn a_turn_is_allowed_until_the_allowance_runs_out() {
        assert!(Allowance::decide(Some(1000), used(0)).allowed);
        assert!(Allowance::decide(Some(1000), used(999)).allowed);
        // The thousandth turn is the last one included, so at 1000
        // used there is nothing left.
        assert!(!Allowance::decide(Some(1000), used(1000)).allowed);
        assert!(!Allowance::decide(Some(1000), used(1001)).allowed);
    }

    #[test]
    fn running_out_points_at_the_free_way_to_carry_on() {
        // A cap is only safe to enforce because the escape hatch
        // exists. If the message does not mention it, it reads as a
        // product that has stopped working.
        let spent = Allowance::decide(Some(1000), used(1000));
        let notice = spent.notice.unwrap();

        assert!(notice.contains("1000"), "{notice}");
        assert!(notice.contains("own API key"), "{notice}");
        assert!(notice.contains("no limit"), "{notice}");
    }

    #[test]
    fn not_being_included_is_not_a_failure() {
        let none = Allowance::decide(None, used(0));

        assert!(!none.allowed);
        assert_eq!(none.included, None);
        let notice = none.notice.unwrap();
        assert!(notice.contains("own API key"), "{notice}");
        // Nothing here should sound like a fault.
        for alarming in ["error", "failed", "denied", "forbidden"] {
            assert!(!notice.to_lowercase().contains(alarming), "{notice}");
        }
    }

    #[test]
    fn the_warning_arrives_near_the_end_and_not_before() {
        assert!(Allowance::decide(Some(100), used(89)).notice.is_none());
        assert!(Allowance::decide(Some(100), used(90)).notice.is_some());
    }

    #[test]
    fn a_period_is_the_calendar_month_it_falls_in() {
        // 2026-06-15T12:34:56Z
        let mid_june = 1_781_354_096_000;
        let start = period_start(mid_june);

        // 2026-06-01T00:00:00Z
        assert_eq!(start, 1_780_272_000_000);
        // Idempotent: the start of a month is its own period start.
        assert_eq!(period_start(start), start);
        // And two instants in the same month agree.
        assert_eq!(period_start(start + 86_400_000 * 20), start);
        // While the next month does not.
        assert_ne!(period_start(start + 86_400_000 * 40), start);
    }

    #[test]
    fn either_assistant_price_includes_it() {
        // Monthly and annual are different prices for the same plan.
        let assistant = ["price_ai_monthly", "price_ai_annual"];

        for price in assistant {
            assert_eq!(
                Plan::for_price(Some(price), &assistant),
                Plan::SyncAndAssistant,
                "{price}"
            );
        }
        assert_eq!(
            Plan::for_price(Some("price_sync_monthly"), &assistant),
            Plan::Sync
        );
    }

    #[test]
    fn anything_unrecognised_fails_closed() {
        // A price this build has never seen, a subscription recorded
        // before prices were distinguished, or a deployment that sells
        // no assistant at all. None may hand out model usage nobody
        // is paying for.
        let assistant = ["price_ai_monthly", "price_ai_annual"];

        assert_eq!(Plan::for_price(None, &assistant), Plan::Sync);
        assert_eq!(
            Plan::for_price(Some("price_unknown"), &assistant),
            Plan::Sync
        );
        // And a deployment selling no assistant cannot be talked into
        // one by any price id at all.
        assert_eq!(Plan::for_price(Some("price_ai_annual"), &[]), Plan::Sync);
        assert!(!Plan::for_price(Some("price_ai_annual"), &[]).includes_assistant());
    }

    #[test]
    fn the_store_does_not_decide_whether_the_assistant_is_included() {
        // What a price means is this deployment's configuration, not a
        // property of the row, so the entitlement the store builds
        // leaves it unset and the handler fills it in.
        let subscription = Subscription {
            workspace_id: "w1".into(),
            customer_id: "cus_1".into(),
            subscription_id: None,
            status: SubscriptionStatus::Active,
            seats: 3,
            current_period_end: None,
            price_id: Some("price_with_ai".into()),
        };

        assert_eq!(subscription.entitlement().assistant_turns, None);
        assert_eq!(
            subscription.plan(&["price_with_ai"]),
            Plan::SyncAndAssistant
        );
    }

    #[test]
    fn self_hosting_is_never_limited() {
        let unmetered = Entitlement::unmetered();
        assert!(unmetered.allows(0));
        assert!(unmetered.allows(10_000));
        assert!(unmetered.active);
    }

    #[test]
    fn seats_bound_growth_and_nothing_else() {
        let three = Entitlement {
            seats: Some(3),
            active: true,
            notice: None,
            assistant_turns: None,
            syncs: true,
        };

        assert!(three.allows(2));
        assert!(!three.allows(3), "a fourth member on three seats");
        // Already over, because somebody reduced the seat count: still
        // refused for growth, but nothing says their data is gone.
        assert!(!three.allows(9));
    }

    #[test]
    fn a_failed_payment_does_not_lock_anyone_out() {
        // Stripe retries for days, and the usual cause is a bank's
        // fraud check rather than a real problem. Locking a team out of
        // their work over it would be the worse mistake.
        assert!(SubscriptionStatus::PastDue.entitles());
        assert!(
            SubscriptionStatus::PastDue
                .notice()
                .unwrap()
                .contains("still works")
        );
    }

    #[test]
    fn a_cancelled_subscription_stops_growth_but_says_the_data_is_safe() {
        assert!(!SubscriptionStatus::Canceled.entitles());
        let notice = SubscriptionStatus::Canceled.notice().unwrap();
        assert!(notice.contains("data is untouched"), "{notice}");
        assert!(notice.contains("still syncs"), "{notice}");
    }

    #[test]
    fn a_status_this_build_does_not_know_does_not_block_anything() {
        // Stripe adds statuses. Treating an unknown one as cancelled
        // would lock people out on somebody else's release schedule.
        let unknown = SubscriptionStatus::parse("some_future_status");
        assert_eq!(unknown, SubscriptionStatus::Unknown);
        assert!(!unknown.entitles());
        assert!(unknown.notice().unwrap().contains("Nothing is blocked"));
    }

    #[test]
    fn stripes_spellings_are_understood() {
        assert_eq!(
            SubscriptionStatus::parse("active"),
            SubscriptionStatus::Active
        );
        assert_eq!(
            SubscriptionStatus::parse("trialing"),
            SubscriptionStatus::Trialing
        );
        assert_eq!(
            SubscriptionStatus::parse("unpaid"),
            SubscriptionStatus::PastDue
        );
        assert_eq!(
            SubscriptionStatus::parse("incomplete_expired"),
            SubscriptionStatus::Canceled
        );
    }
}
