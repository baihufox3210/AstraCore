//! Authorization for operations requested through the asusio daemon.
//!
//! The socket's file mode is the first gate; this policy is checked again per request
//! using the peer's kernel-reported credentials.
use std::fmt;

/// Kernel-reported identity of a connected client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    /// Primary and supplementary group IDs.
    pub groups: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Read daemon, restore and fan-curve status.
    ReadStatus,
    /// Resolve a lighting plan without writing hardware.
    PreviewLighting,
    /// Write lighting and record it for restoration at boot.
    SetLighting,
    /// Reapply the recorded lighting state.
    RestoreLighting,
}

impl Operation {
    pub fn writes_hardware(self) -> bool {
        matches!(self, Self::SetLighting | Self::RestoreLighting)
    }

    fn describe(self) -> &'static str {
        match self {
            Self::ReadStatus => "read daemon status",
            Self::PreviewLighting => "preview lighting",
            Self::SetLighting => "set lighting",
            Self::RestoreLighting => "restore lighting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub uid: u32,
    pub operation: Operation,
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "uid {} is not allowed to {}; add the user to the daemon's client group",
            self.uid,
            self.operation.describe()
        )
    }
}

impl std::error::Error for Denied {}

/// Root and the daemon's own user may do everything. Members of the client group may
/// read status and control lighting. Without a client group, nobody else is allowed.
#[derive(Debug, Clone)]
pub struct Policy {
    owner_uid: u32,
    client_gid: Option<u32>,
}

impl Policy {
    pub fn new(owner_uid: u32, client_gid: Option<u32>) -> Self {
        Self {
            owner_uid,
            client_gid,
        }
    }

    pub fn authorize(&self, caller: &Caller, operation: Operation) -> Result<(), Denied> {
        let trusted = caller.uid == 0 || caller.uid == self.owner_uid;
        let member = self
            .client_gid
            .is_some_and(|gid| caller.groups.contains(&gid));
        if trusted || member {
            Ok(())
        } else {
            Err(Denied {
                uid: caller.uid,
                operation,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(uid: u32, groups: &[u32]) -> Caller {
        Caller {
            uid,
            groups: groups.to_vec(),
        }
    }

    #[test]
    fn root_owner_and_group_members_are_allowed() {
        let policy = Policy::new(0, Some(963));
        assert!(
            policy
                .authorize(&caller(0, &[0]), Operation::SetLighting)
                .is_ok()
        );
        assert!(
            policy
                .authorize(&caller(1000, &[1000, 963]), Operation::SetLighting)
                .is_ok()
        );
        let denied = policy
            .authorize(&caller(1001, &[1001]), Operation::RestoreLighting)
            .unwrap_err();
        assert!(denied.to_string().contains("uid 1001"));
        assert!(Operation::SetLighting.writes_hardware());
        assert!(!Operation::PreviewLighting.writes_hardware());
    }

    #[test]
    fn without_a_client_group_only_trusted_users_are_allowed() {
        let policy = Policy::new(1000, None);
        assert!(
            policy
                .authorize(&caller(1000, &[1000]), Operation::SetLighting)
                .is_ok()
        );
        assert!(
            policy
                .authorize(&caller(1001, &[1001]), Operation::ReadStatus)
                .is_err()
        );
    }
}
