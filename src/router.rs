use crate::config::RoutingPolicy;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct RouteCandidate {
    pub account_id: String,
    pub in_flight_requests: u32,
    pub last_selected_at: Option<DateTime<Utc>>,
}

pub fn select_candidate(
    policy: RoutingPolicy,
    candidates: &[RouteCandidate],
    round_robin_cursor: &mut usize,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }

    match policy {
        RoutingPolicy::RoundRobin => {
            let index = *round_robin_cursor % candidates.len();
            *round_robin_cursor = round_robin_cursor.saturating_add(1);
            candidates
                .get(index)
                .map(|candidate| candidate.account_id.clone())
        }
        RoutingPolicy::LeastInFlight => candidates
            .iter()
            .min_by(|left, right| {
                left.in_flight_requests
                    .cmp(&right.in_flight_requests)
                    .then_with(|| left.last_selected_at.cmp(&right.last_selected_at))
                    .then_with(|| left.account_id.cmp(&right.account_id))
            })
            .map(|candidate| candidate.account_id.clone()),
        RoutingPolicy::FillFirst => candidates
            .iter()
            .min_by(|left, right| left.account_id.cmp(&right.account_id))
            .map(|candidate| candidate.account_id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_first_picks_stable_first_candidate() {
        let candidates = vec![
            RouteCandidate {
                account_id: "b-account".to_string(),
                in_flight_requests: 99,
                last_selected_at: Some(Utc::now()),
            },
            RouteCandidate {
                account_id: "a-account".to_string(),
                in_flight_requests: 0,
                last_selected_at: None,
            },
        ];

        let mut cursor = 0;
        let selected =
            select_candidate(RoutingPolicy::FillFirst, &candidates, &mut cursor).unwrap();

        assert_eq!(selected, "a-account");
    }
}
