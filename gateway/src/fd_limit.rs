use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaisePlan {
    AlreadyAtHard { limit: u64 },
    Raise { from: u64, to: u64 },
}

pub fn plan_raise(soft: u64, hard: u64) -> RaisePlan {
    if soft >= hard {
        RaisePlan::AlreadyAtHard { limit: soft }
    } else {
        RaisePlan::Raise {
            from: soft,
            to: hard,
        }
    }
}

pub fn describe_limit(v: u64) -> String {
    if v == unlimited() {
        "unlimited".to_string()
    } else {
        v.to_string()
    }
}

const fn unlimited() -> u64 {
    u64::MAX
}

fn to_u64_limit(value: libc::rlim_t, label: &'static str) -> io::Result<u64> {
    if value == libc::RLIM_INFINITY {
        Ok(u64::MAX)
    } else {
        u64::try_from(value).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("negative {label} limit"),
            )
        })
    }
}

fn from_u64_limit(value: u64, label: &'static str) -> io::Result<libc::rlim_t> {
    if value == u64::MAX {
        Ok(libc::RLIM_INFINITY)
    } else {
        libc::rlim_t::try_from(value).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{label} limit does not fit libc::rlim_t"),
            )
        })
    }
}

pub fn raise_nofile_soft_to_hard() -> Option<u64> {
    let (soft, hard) = match read_nofile() {
        Ok(pair) => pair,
        Err(e) => {
            log::warn!(
                "fd limit: could not read RLIMIT_NOFILE ({e}) — continuing with whatever limit was \
                 inherited. If sessions die with \"Too many open files\", raise it outside the \
                 process (`ulimit -n`, or docker `--ulimit nofile=65536`)."
            );
            return None;
        }
    };

    match plan_raise(soft, hard) {
        RaisePlan::AlreadyAtHard { limit } => {
            log::info!(
                "fd limit: RLIMIT_NOFILE soft is already at the hard limit ({}) — nothing to raise",
                describe_limit(limit)
            );
            Some(limit)
        }
        RaisePlan::Raise { from, to } => match set_nofile_soft(to, hard) {
            Ok(()) => {
                log::info!(
                    "fd limit: RLIMIT_NOFILE soft raised {} -> {} (hard {})",
                    describe_limit(from),
                    describe_limit(to),
                    describe_limit(hard)
                );
                Some(to)
            }
            Err(e) => {
                log::warn!(
                    "fd limit: could not raise RLIMIT_NOFILE soft {} -> {} ({e}) — continuing at \
                     {}. Each World Session needs three client descriptors plus database \
                     sockets. Set LYRACORE_MAX_SESSIONS within this descriptor budget.",
                    describe_limit(from),
                    describe_limit(to),
                    describe_limit(from)
                );
                Some(from)
            }
        },
    }
}

fn read_nofile() -> io::Result<(u64, u64)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only to our live, initialized rlimit for the duration of the call.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let soft = to_u64_limit(lim.rlim_cur, "soft RLIMIT_NOFILE")?;
    let hard = to_u64_limit(lim.rlim_max, "hard RLIMIT_NOFILE")?;
    Ok((soft, hard))
}

fn set_nofile_soft(soft: u64, hard: u64) -> io::Result<()> {
    let soft = from_u64_limit(soft, "soft RLIMIT_NOFILE")?;
    let hard = from_u64_limit(hard, "hard RLIMIT_NOFILE")?;
    let lim = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads only from our live, initialized rlimit for the duration of the call.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infinity_maps_to_unlimited_and_back() {
        let raw = libc::RLIM_INFINITY;
        assert_eq!(to_u64_limit(raw, "test").unwrap(), unlimited());
        assert_eq!(from_u64_limit(unlimited(), "test").unwrap(), raw);
        assert_eq!(
            describe_limit(to_u64_limit(raw, "test").unwrap()),
            "unlimited"
        );
    }

    #[test]
    fn finite_limits_cross_the_libc_boundary_unchanged() {
        for value in [0_u64, 1024, 524_288] {
            let raw = from_u64_limit(value, "test").unwrap();
            assert_eq!(to_u64_limit(raw, "test").unwrap(), value);
        }
    }

    #[test]
    fn the_default_docker_shape_gets_raised() {
        assert_eq!(
            plan_raise(1024, 524_288),
            RaisePlan::Raise {
                from: 1024,
                to: 524_288
            }
        );
    }

    #[test]
    fn an_already_raised_limit_is_left_alone() {
        assert_eq!(
            plan_raise(65_536, 65_536),
            RaisePlan::AlreadyAtHard { limit: 65_536 }
        );
        assert_eq!(
            plan_raise(65_536, 1024),
            RaisePlan::AlreadyAtHard { limit: 65_536 }
        );
    }

    #[test]
    fn an_unlimited_hard_limit_is_still_a_raise_when_soft_is_lower() {
        let inf = unlimited();
        assert_eq!(
            plan_raise(1024, inf),
            RaisePlan::Raise {
                from: 1024,
                to: inf
            }
        );
        assert_eq!(
            plan_raise(inf, inf),
            RaisePlan::AlreadyAtHard { limit: inf }
        );
    }

    #[test]
    fn unlimited_reads_as_a_word_not_as_u64_max() {
        assert_eq!(describe_limit(unlimited()), "unlimited");
        assert_eq!(describe_limit(1024), "1024");
        assert_eq!(describe_limit(524_288), "524288");
    }

    #[test]
    fn raising_for_real_never_lowers_and_reaches_the_hard_limit_when_permitted() {
        let (soft_before, hard) = read_nofile().expect("getrlimit on our own process");
        let reported = raise_nofile_soft_to_hard().expect("getrlimit must not fail on ourselves");

        let (soft_after, hard_after) = read_nofile().expect("getrlimit on our own process");
        assert_eq!(
            soft_after, reported,
            "the reported limit is the one in force"
        );
        assert!(
            soft_after >= soft_before,
            "the soft limit must never be lowered ({soft_before} -> {soft_after})"
        );
        assert_eq!(hard_after, hard, "the hard limit must not move");
        if soft_after != hard {
            let e = set_nofile_soft(hard, hard).expect_err(
                "setrlimit succeeded when called directly but raise_nofile_soft_to_hard left the \
                 soft limit below hard — that is a bug here, not a sandbox policy",
            );
            eprintln!("setrlimit is denied in this environment ({e}); tolerated, see the doc");
        }
    }
}
