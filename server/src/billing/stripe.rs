//! Talking to Stripe.
//!
//! Four things are needed and no more: start a checkout, open the
//! customer portal, verify a webhook (next door, in [`super::webhook`])
//! and read a subscription out of an event. Raw HTTP rather than a
//! crate modelling the whole API — the one candidate is at a release
//! candidate, and this is a few form posts.

use std::time::Duration;

use serde_json::Value;

use crate::error::{ServerError, ServerResult};

/// Stripe's API host.
const API: &str = "https://api.stripe.com";

/// A configured Stripe account.
pub struct Stripe {
    client: reqwest::Client,
    secret_key: String,
    /// Price details, read once each. Stripe's amounts are
    /// immutable — a change means a new price id — so nothing here
    /// can go stale.
    prices: std::sync::Mutex<std::collections::HashMap<String, PriceDetail>>,
    base_url: String,
}

/// What a price costs, as Stripe reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceDetail {
    /// Per seat, per interval, in the currency's smallest unit.
    pub amount: i64,
    /// ISO 4217, lower case.
    pub currency: String,
    /// `month` or `year`.
    pub interval: String,
}

impl std::fmt::Debug for Stripe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No secret_key field: this ends up in spans and panics.
        f.debug_struct("Stripe").finish_non_exhaustive()
    }
}

impl Stripe {
    /// Builds a client.
    ///
    /// No price: the prices live in the configuration, which knows
    /// how many there are and what each one means. This knows how to
    /// talk to Stripe.
    pub fn new(secret_key: impl Into<String>) -> ServerResult<Self> {
        let secret_key = secret_key.into();

        if secret_key.trim().is_empty() {
            return Err(ServerError::Config("STRIPE_SECRET_KEY is empty".into()));
        }

        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| ServerError::Config(e.to_string()))?,
            secret_key,
            prices: Default::default(),
            base_url: API.into(),
        })
    }

    /// What a price actually costs, as Stripe reports it.
    ///
    /// Cached for the life of the process. A price is changed by
    /// creating a new one — Stripe's own amounts are immutable — so
    /// there is nothing to go stale, and the alternative is four API
    /// calls every time somebody opens the billing panel.
    pub async fn price(&self, price_id: &str) -> ServerResult<PriceDetail> {
        if let Some(known) = self.prices.lock().ok().and_then(|p| p.get(price_id).cloned()) {
            return Ok(known);
        }

        let body = self.get(&format!("/v1/prices/{price_id}")).await?;
        let detail = PriceDetail {
            // `unit_amount` is null for tiered pricing, which Relay
            // does not sell. Zero is wrong but visible, where a
            // guess would be wrong and plausible.
            amount: body["unit_amount"].as_i64().unwrap_or(0),
            currency: body["currency"].as_str().unwrap_or("gbp").to_string(),
            interval: body["recurring"]["interval"]
                .as_str()
                .unwrap_or("month")
                .to_string(),
        };

        if let Ok(mut cache) = self.prices.lock() {
            cache.insert(price_id.to_string(), detail.clone());
        }
        Ok(detail)
    }

    /// Points the client at something other than Stripe.
    ///
    /// For `stripe-mock` and for tests: a test that reached the real
    /// API would need a live key and would create real objects, so
    /// the checkout route would simply go untested instead.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Starts a checkout for a workspace, returning the URL to open.
    ///
    /// `idempotency` is the caller's guard against a double-click
    /// becoming two subscriptions: Stripe returns the first session
    /// again rather than making another.
    #[allow(clippy::too_many_arguments)]
    pub async fn checkout_session(
        &self,
        workspace_id: &str,
        price_id: &str,
        seats: u32,
        customer_email: &str,
        success_url: &str,
        cancel_url: &str,
        idempotency: &str,
    ) -> ServerResult<String> {
        // Stripe is form-encoded, with brackets for nesting.
        let seats = seats.max(1).to_string();
        let form = [
            ("mode", "subscription"),
            ("line_items[0][price]", price_id),
            ("line_items[0][quantity]", seats.as_str()),
            ("success_url", success_url),
            ("cancel_url", cancel_url),
            ("customer_email", customer_email),
            // Both, deliberately. client_reference_id comes back on
            // checkout.session.completed; the subscription metadata
            // comes back on every later subscription event, which is
            // the only way to know whose workspace one belongs to.
            ("client_reference_id", workspace_id),
            ("subscription_data[metadata][workspace_id]", workspace_id),
            ("metadata[workspace_id]", workspace_id),
        ];

        let body = self
            .post("/v1/checkout/sessions", &form, Some(idempotency))
            .await?;

        body["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ServerError::Stripe("returned a session with no URL".into()))
    }

    /// Opens the customer portal, returning the URL.
    pub async fn portal_session(
        &self,
        customer_id: &str,
        return_url: &str,
    ) -> ServerResult<String> {
        let form = [("customer", customer_id), ("return_url", return_url)];
        let body = self
            .post("/v1/billing_portal/sessions", &form, None)
            .await?;

        body["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ServerError::Stripe("returned a portal with no URL".into()))
    }

    /// A plain GET, for reading an object back.
    async fn get(&self, path: &str) -> ServerResult<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret_key)
            .send()
            .await
            .map_err(|e| ServerError::Stripe(format!("could not be reached: {e}")))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            return Err(ServerError::Stripe(format!("{status}: {detail}")));
        }

        serde_json::from_str(&body)
            .map_err(|e| ServerError::Stripe(format!("sent something unreadable: {e}")))
    }

    async fn post(
        &self,
        path: &str,
        form: &[(&str, &str)],
        idempotency: Option<&str>,
    ) -> ServerResult<Value> {
        let mut request = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.secret_key)
            .form(form);

        if let Some(key) = idempotency {
            request = request.header("Idempotency-Key", key);
        }

        let response = request
            .send()
            .await
            .map_err(|e| ServerError::Stripe(format!("could not be reached: {e}")))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            // Stripe's own message is the operator's best clue — "no
            // such price", "testmode key against a live price" — and
            // it carries no card data.
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);

            return Err(ServerError::Stripe(format!("{status}: {detail}")));
        }

        serde_json::from_str(&body)
            .map_err(|e| ServerError::Stripe(format!("sent something unreadable: {e}")))
    }
}

/// What a webhook is telling us about a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOutcome {
    /// Stripe's event id, for the replay guard.
    pub event_id: String,
    /// Which workspace, when the event says.
    pub workspace_id: Option<String>,
    /// Stripe's customer id.
    pub customer_id: Option<String>,
    /// The subscription, when the event carries one.
    pub subscription_id: Option<String>,
    /// How it stands.
    pub status: Option<super::SubscriptionStatus>,
    /// Seats, from the first line item's quantity.
    pub seats: Option<u32>,
    /// The price the subscription is on, which is what says whether
    /// the assistant is included.
    pub price_id: Option<String>,
    /// When the paid period ends, in epoch milliseconds.
    pub current_period_end: Option<i64>,
}

/// Reads an event body into the parts that matter.
///
/// Deliberately tolerant: Stripe sends dozens of event types and adds
/// more, and an event this build does not understand should be
/// acknowledged and ignored rather than retried for ever. A `None`
/// workspace means "nothing to apply".
pub fn read_event(body: &str) -> ServerResult<EventOutcome> {
    let event: Value = serde_json::from_str(body)
        .map_err(|e| ServerError::BadRequest(format!("the webhook body was not JSON: {e}")))?;

    let object = &event["data"]["object"];
    let kind = event["type"].as_str().unwrap_or_default();

    // The workspace id travels in metadata, because a subscription
    // event names a customer and a customer means nothing here.
    let workspace_id = object["metadata"]["workspace_id"]
        .as_str()
        .or_else(|| object["client_reference_id"].as_str())
        .map(str::to_string);

    let status = match kind {
        // A completed checkout is not yet a subscription event; treat
        // it as active so the workspace works immediately rather than
        // waiting for the subscription webhook that follows.
        "checkout.session.completed" => Some(super::SubscriptionStatus::Active),
        "customer.subscription.deleted" => Some(super::SubscriptionStatus::Canceled),
        kind if kind.starts_with("customer.subscription.") => Some(
            super::SubscriptionStatus::parse(object["status"].as_str().unwrap_or_default()),
        ),
        _ => None,
    };

    Ok(EventOutcome {
        event_id: event["id"].as_str().unwrap_or_default().to_string(),
        workspace_id,
        customer_id: object["customer"].as_str().map(str::to_string),
        subscription_id: object["subscription"]
            .as_str()
            .or_else(|| {
                // On a subscription event the object *is* the
                // subscription, so its own id is the one.
                kind.starts_with("customer.subscription.")
                    .then(|| object["id"].as_str())
                    .flatten()
            })
            .map(str::to_string),
        status,
        seats: object["items"]["data"][0]["quantity"]
            .as_u64()
            .map(|q| q as u32),
        price_id: object["items"]["data"][0]["price"]["id"]
            .as_str()
            .map(str::to_string),
        // Stripe counts in seconds; everything here counts in millis.
        current_period_end: object["current_period_end"]
            .as_i64()
            .map(|seconds| seconds * 1000),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::SubscriptionStatus;

    #[test]
    fn a_client_needs_a_key() {
        // The prices moved to the configuration, which knows there
        // are four of them and what each one means.
        assert!(Stripe::new("").is_err());
        assert!(Stripe::new("   ").is_err());
        assert!(Stripe::new("sk_test_1").is_ok());
    }

    #[test]
    fn debug_output_does_not_contain_the_secret_key() {
        let stripe = Stripe::new("sk_test_very_secret").unwrap();
        assert!(!format!("{stripe:?}").contains("very_secret"));
    }

    #[test]
    fn a_completed_checkout_names_its_workspace_and_counts_as_active() {
        let outcome = read_event(
            r#"{"id":"evt_1","type":"checkout.session.completed","data":{"object":{
                "client_reference_id":"w1","customer":"cus_1","subscription":"sub_1"
            }}}"#,
        )
        .unwrap();

        assert_eq!(outcome.workspace_id.as_deref(), Some("w1"));
        assert_eq!(outcome.customer_id.as_deref(), Some("cus_1"));
        assert_eq!(outcome.subscription_id.as_deref(), Some("sub_1"));
        // Active immediately, so the workspace works without waiting
        // for the subscription event that follows.
        assert_eq!(outcome.status, Some(SubscriptionStatus::Active));
    }

    #[test]
    fn a_subscription_event_carries_the_workspace_in_metadata() {
        // A subscription event names a customer, and a customer means
        // nothing here without the metadata put on at checkout.
        let outcome = read_event(
            r#"{"id":"evt_2","type":"customer.subscription.updated","data":{"object":{
                "id":"sub_1","customer":"cus_1","status":"past_due",
                "current_period_end":1700000000,
                "metadata":{"workspace_id":"w1"},
                "items":{"data":[{"quantity":7}]}
            }}}"#,
        )
        .unwrap();

        assert_eq!(outcome.workspace_id.as_deref(), Some("w1"));
        assert_eq!(outcome.subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(outcome.status, Some(SubscriptionStatus::PastDue));
        assert_eq!(outcome.seats, Some(7));
        // Stripe counts seconds; everything here counts milliseconds.
        assert_eq!(outcome.current_period_end, Some(1_700_000_000_000));
    }

    #[test]
    fn a_subscription_event_carries_the_price_it_is_on() {
        // Which plan a workspace is on is read from this, so losing
        // it would quietly drop everyone to the cheaper tier.
        let outcome = read_event(
            r#"{"id":"evt_p","type":"customer.subscription.updated",
                "data":{"object":{"id":"sub_1","customer":"cus_1","status":"active",
                "metadata":{"workspace_id":"w1"},
                "items":{"data":[{"quantity":4,"price":{"id":"price_with_ai"}}]}}}}"#,
        )
        .unwrap();

        assert_eq!(outcome.price_id.as_deref(), Some("price_with_ai"));
        assert_eq!(outcome.seats, Some(4));
    }

    #[test]
    fn an_event_without_a_price_says_so_rather_than_inventing_one() {
        let outcome = read_event(
            r#"{"id":"evt_c","type":"checkout.session.completed",
                "data":{"object":{"customer":"cus_1","client_reference_id":"w1"}}}"#,
        )
        .unwrap();

        assert_eq!(outcome.price_id, None);
    }

    #[test]
    fn a_deleted_subscription_is_cancelled_whatever_its_status_field_says() {
        let outcome = read_event(
            r#"{"id":"evt_3","type":"customer.subscription.deleted","data":{"object":{
                "id":"sub_1","status":"active","metadata":{"workspace_id":"w1"}
            }}}"#,
        )
        .unwrap();

        assert_eq!(outcome.status, Some(SubscriptionStatus::Canceled));
    }

    #[test]
    fn an_event_this_build_does_not_know_is_readable_and_applies_nothing() {
        // Stripe sends dozens of types and adds more. One we do not
        // understand must be acknowledged, not retried for ever.
        let outcome =
            read_event(r#"{"id":"evt_4","type":"invoice.payment_succeeded","data":{"object":{}}}"#)
                .unwrap();

        assert_eq!(outcome.event_id, "evt_4");
        assert_eq!(outcome.status, None);
        assert_eq!(outcome.workspace_id, None);
    }

    #[test]
    fn a_body_that_is_not_json_is_a_bad_request_not_a_panic() {
        assert!(read_event("not json at all").is_err());
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let outcome =
            read_event(r#"{"id":"evt_5","type":"customer.subscription.updated"}"#).unwrap();
        assert_eq!(outcome.event_id, "evt_5");
        assert_eq!(outcome.seats, None);
        assert_eq!(outcome.current_period_end, None);
    }
}
