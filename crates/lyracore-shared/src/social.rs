#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactRefusal {
    ActorUnavailable,
    AddSelf,
    AlreadyOnList,
    ListFull,
    NotOnList,
    Enemy,
}

impl ContactRefusal {
    pub const ALL: [Self; 6] = [
        Self::ActorUnavailable,
        Self::AddSelf,
        Self::AlreadyOnList,
        Self::ListFull,
        Self::NotOnList,
        Self::Enemy,
    ];

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::ActorUnavailable => "social:actor_unavailable",
            Self::AddSelf => "social:add_self",
            Self::AlreadyOnList => "social:already_on_list",
            Self::ListFull => "social:list_full",
            Self::NotOnList => "social:not_on_list",
            Self::Enemy => "social:enemy",
        }
    }

    pub fn parse_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.as_tag() == tag)
    }
}

#[cfg(test)]
mod tests {
    use super::ContactRefusal;

    #[test]
    fn every_contact_refusal_tag_round_trips() {
        for refusal in ContactRefusal::ALL {
            assert_eq!(ContactRefusal::parse_tag(refusal.as_tag()), Some(refusal));
        }
        assert_eq!(ContactRefusal::parse_tag("social:"), None);
        assert_eq!(
            ContactRefusal::parse_tag("gw_add_friend reducer timed out after 10s"),
            None
        );
    }
}
