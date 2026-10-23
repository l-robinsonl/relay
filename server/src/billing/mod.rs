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
}

impl Entitlement {
    /// No limits. What self-hosting always gets.
    pub fn unmetered() -> Self {
        Self {
            seats: None,
            active: true,
            notice: None,
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
}

impl Subscription {
    /// What this subscription entitles.
    pub fn entitlement(&self) -> Entitlement {
        Entitlement {
            seats: Some(self.seats),
            active: self.status.entitles(),
            notice: self.status.notice(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
