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
    pub fn for_price(price_id: Option<&str>, assistant_price: Option<&str>) -> Self {
        match (price_id, assistant_price) {
            (Some(price), Some(assistant)) if price == assistant => Self::SyncAndAssistant,
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
        }
    }

    /// Which plan this is, given what the deployment charges for.
    pub fn plan(&self, assistant_price: Option<&str>) -> Plan {
        Plan::for_price(self.price_id.as_deref(), assistant_price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assistant_price_is_the_only_one_that_includes_it() {
        let assistant = Some("price_with_ai");

        assert_eq!(
            Plan::for_price(Some("price_with_ai"), assistant),
            Plan::SyncAndAssistant
        );
        assert_eq!(Plan::for_price(Some("price_sync"), assistant), Plan::Sync);
    }

    #[test]
    fn anything_unrecognised_fails_closed() {
        // A price this build has never seen, a subscription recorded
        // before prices were distinguished, or a deployment that sells
        // no assistant at all. None of them may hand out model usage
        // nobody is paying for.
        let assistant = Some("price_with_ai");

        assert_eq!(Plan::for_price(None, assistant), Plan::Sync);
        assert_eq!(
            Plan::for_price(Some("price_unknown"), assistant),
            Plan::Sync
        );
        // And a deployment that has configured no assistant price
        // cannot be talked into one by any price id at all.
        assert_eq!(Plan::for_price(Some("price_with_ai"), None), Plan::Sync);
        assert!(!Plan::for_price(Some("price_with_ai"), None).includes_assistant());
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
            subscription.plan(Some("price_with_ai")),
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
