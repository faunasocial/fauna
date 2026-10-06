use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryStatus {
    Queued,
    SentP2P,
    SentNest,
    WakeSent,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryPath {
    DirectP2P,
    RecipientNest,
    OwnNestDeposit,
    QueueLocal,
}

pub fn delivery_decision(
    tunnel_active: bool,
    recipient_has_nest: bool,
    sender_has_nest: bool,
) -> DeliveryPath {
    if tunnel_active {
        DeliveryPath::DirectP2P
    } else if recipient_has_nest {
        DeliveryPath::RecipientNest
    } else if sender_has_nest {
        DeliveryPath::OwnNestDeposit
    } else {
        DeliveryPath::QueueLocal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefer_active_tunnel() {
        assert_eq!(delivery_decision(true, true, true), DeliveryPath::DirectP2P);
    }

    #[test]
    fn fallback_to_nest_when_no_tunnel() {
        assert_eq!(
            delivery_decision(false, true, true),
            DeliveryPath::RecipientNest
        );
    }

    #[test]
    fn deposit_on_own_nest_when_recipient_unreachable() {
        assert_eq!(
            delivery_decision(false, false, true),
            DeliveryPath::OwnNestDeposit
        );
    }

    #[test]
    fn queue_locally_when_no_nests() {
        assert_eq!(
            delivery_decision(false, false, false),
            DeliveryPath::QueueLocal
        );
    }
}
