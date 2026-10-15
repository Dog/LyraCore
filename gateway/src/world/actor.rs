use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Actor(NonZeroU64);

impl Actor {
    pub(crate) fn new(guid: u64) -> Option<Self> {
        NonZeroU64::new(guid).map(Self)
    }

    pub(crate) fn guid(self) -> u64 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::Actor;

    #[test]
    fn guid_zero_is_no_actor() {
        assert_eq!(Actor::new(0), None);
        assert_eq!(Actor::new(7).map(Actor::guid), Some(7));
    }
}
