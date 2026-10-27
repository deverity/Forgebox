//! `ports` table: logical port reservations ([`PortReservation`], docs/03 §4).
//!
//! The table enforces one reservation per `(protocol, port)` and per owner port
//! (`(owner_kind, owner_id, owner_port_name)`); [`PortRepo::insert`] reports either
//! collision as `conflict`. Allocation itself (choosing a free port of a range) is
//! `forge_net::ports::PortAllocator`, which runs the choice and the insert in one write
//! operation of the store's writer thread.

use std::collections::BTreeSet;

use rusqlite::{Connection, ErrorCode, Row, params};

use crate::Timestamp;
use crate::error::{ForgeError, Result};
use crate::ids::PortReservationId;
use crate::model::port::{
    OwnerKind, PortHolder, PortProtocol, PortReservation, PortState, ReservationOwner,
};

const COLUMNS: &str = "id, port, protocol, range_name, owner_kind, owner_id, owner_port_name, \
                       state, conflict_pid, conflict_exe, created_at, updated_at";

/// Repository over the `ports` table.
#[derive(Debug, Clone, Copy)]
pub struct PortRepo<'c> {
    conn: &'c Connection,
}

/// Raw columns of a row, converted outside rusqlite's closure (errors become `db`).
struct RawRow {
    id: PortReservationId,
    port: i64,
    protocol: String,
    range: Option<String>,
    owner_kind: String,
    owner_id: String,
    owner_port_name: String,
    state: String,
    conflict_pid: Option<i64>,
    conflict_exe: Option<String>,
    created_at: String,
    updated_at: String,
}

fn raw(row: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: row.get(0)?,
        port: row.get(1)?,
        protocol: row.get(2)?,
        range: row.get(3)?,
        owner_kind: row.get(4)?,
        owner_id: row.get(5)?,
        owner_port_name: row.get(6)?,
        state: row.get(7)?,
        conflict_pid: row.get(8)?,
        conflict_exe: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

impl RawRow {
    fn into_reservation(self) -> Result<PortReservation> {
        let id = self.id;
        let corrupt = |what: &str, e: String| {
            ForgeError::Schema(format!("ports row {id}: invalid {what}: {e}"))
        };
        let ts = |text: &str, what: &str| {
            text.parse::<Timestamp>()
                .map_err(|e| corrupt(what, e.to_string()))
        };
        let conflict = match self.conflict_pid {
            Some(pid) => Some(PortHolder {
                pid: u32::try_from(pid).map_err(|e| corrupt("conflict_pid", e.to_string()))?,
                exe: self.conflict_exe,
            }),
            None => None,
        };
        Ok(PortReservation {
            id,
            port: u16::try_from(self.port).map_err(|e| corrupt("port", e.to_string()))?,
            protocol: self
                .protocol
                .parse()
                .map_err(|e: ForgeError| corrupt("protocol", e.to_string()))?,
            range: self.range,
            owner: ReservationOwner {
                kind: self
                    .owner_kind
                    .parse()
                    .map_err(|e: ForgeError| corrupt("owner_kind", e.to_string()))?,
                id: self.owner_id,
                port_name: self.owner_port_name,
            },
            state: self
                .state
                .parse()
                .map_err(|e: ForgeError| corrupt("state", e.to_string()))?,
            conflict,
            created_at: ts(&self.created_at, "created_at")?,
            updated_at: ts(&self.updated_at, "updated_at")?,
        })
    }
}

impl<'c> PortRepo<'c> {
    /// Repository on `conn` (a writer operation, a reader or the maintenance connection).
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Stores `reservation`. `Conflict` when the port is already reserved (same
    /// protocol) or the owner already holds a port under the same name.
    pub fn insert(&self, reservation: &PortReservation) -> Result<()> {
        reservation.owner.validate()?;
        let r = reservation;
        let result = self.conn.execute(
            &format!("INSERT INTO ports ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"),
            params![
                r.id,
                i64::from(r.port),
                r.protocol.as_str(),
                r.range,
                r.owner.kind.as_str(),
                r.owner.id,
                r.owner.port_name,
                r.state.as_str(),
                r.conflict.as_ref().map(|c| i64::from(c.pid)),
                r.conflict.as_ref().and_then(|c| c.exe.clone()),
                r.created_at.to_string(),
                r.updated_at.to_string(),
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Err(ForgeError::Conflict(format!(
                    "{} port {} or a port named {:?} of {}/{} is already reserved",
                    r.protocol, r.port, r.owner.port_name, r.owner.kind, r.owner.id
                )))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn query(&self, filter: &str, params: impl rusqlite::Params) -> Result<Vec<PortReservation>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM ports {filter}"))?;
        let rows = stmt.query_map(params, raw)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.into_reservation()?);
        }
        Ok(out)
    }

    /// Every reservation, by protocol then port.
    pub fn list(&self) -> Result<Vec<PortReservation>> {
        self.query("ORDER BY protocol, port", [])
    }

    /// Reservations of one owner (all its port names), by port.
    pub fn list_owner(&self, kind: OwnerKind, id: &str) -> Result<Vec<PortReservation>> {
        self.query(
            "WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY port",
            params![kind.as_str(), id],
        )
    }

    pub fn get(&self, id: PortReservationId) -> Result<Option<PortReservation>> {
        Ok(self.query("WHERE id = ?1", [id])?.into_iter().next())
    }

    /// The reservation of `owner` (kind, id and port name).
    pub fn find_owner(&self, owner: &ReservationOwner) -> Result<Option<PortReservation>> {
        Ok(self
            .query(
                "WHERE owner_kind = ?1 AND owner_id = ?2 AND owner_port_name = ?3",
                params![owner.kind.as_str(), owner.id, owner.port_name],
            )?
            .into_iter()
            .next())
    }

    /// The reservation of `port`.
    pub fn find_port(&self, protocol: PortProtocol, port: u16) -> Result<Option<PortReservation>> {
        Ok(self
            .query(
                "WHERE protocol = ?1 AND port = ?2",
                params![protocol.as_str(), i64::from(port)],
            )?
            .into_iter()
            .next())
    }

    /// Reserved port numbers of `protocol`.
    pub fn reserved_ports(&self, protocol: PortProtocol) -> Result<BTreeSet<u16>> {
        let mut stmt = self
            .conn
            .prepare("SELECT port FROM ports WHERE protocol = ?1")?;
        let ports = stmt
            .query_map([protocol.as_str()], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ports
            .into_iter()
            .filter_map(|p| u16::try_from(p).ok())
            .collect())
    }

    /// Updates the state (and the external holder) of a reservation. Returns whether the
    /// row exists.
    pub fn set_state(
        &self,
        id: PortReservationId,
        state: PortState,
        conflict: Option<&PortHolder>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE ports SET state = ?2, conflict_pid = ?3, conflict_exe = ?4, updated_at = ?5 \
             WHERE id = ?1",
            params![
                id,
                state.as_str(),
                conflict.map(|c| i64::from(c.pid)),
                conflict.and_then(|c| c.exe.clone()),
                Timestamp::now().to_string(),
            ],
        )?;
        Ok(changed == 1)
    }

    /// Deletes one reservation; returns whether it existed.
    pub fn delete(&self, id: PortReservationId) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM ports WHERE id = ?1", [id])? == 1)
    }

    /// Deletes the reservation of `owner` (kind, id and port name); returns it.
    pub fn delete_owner_port(&self, owner: &ReservationOwner) -> Result<Option<PortReservation>> {
        let found = self.find_owner(owner)?;
        if let Some(r) = &found {
            self.delete(r.id)?;
        }
        Ok(found)
    }

    /// Deletes every reservation of an owner (instance removed); returns how many.
    pub fn delete_owner(&self, kind: OwnerKind, id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM ports WHERE owner_kind = ?1 AND owner_id = ?2",
            params![kind.as_str(), id],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::store::Store;

    fn owner(id: &str, name: &str) -> ReservationOwner {
        ReservationOwner::new(OwnerKind::Instance, id, name).unwrap()
    }

    #[test]
    fn insert_list_find_and_delete() {
        let store = Store::open_in_memory().unwrap();
        let repo = PortRepo::new(store.conn());
        let mysql = PortReservation::new_tcp(3306, Some("databases".into()), owner("a", "main"));
        repo.insert(&mysql).unwrap();
        let mail = PortReservation::new_tcp(1025, Some("mail".into()), owner("b", "smtp"));
        repo.insert(&mail).unwrap();
        let ui = PortReservation::new_tcp(8025, None, owner("b", "ui"));
        repo.insert(&ui).unwrap();

        let all = repo.list().unwrap();
        assert_eq!(
            all.iter().map(|r| r.port).collect::<Vec<_>>(),
            vec![1025, 3306, 8025]
        );
        assert_eq!(all[1], mysql, "round trip keeps every field");
        assert_eq!(repo.get(mysql.id).unwrap(), Some(mysql.clone()));
        assert_eq!(
            repo.find_owner(&owner("a", "main")).unwrap(),
            Some(mysql.clone())
        );
        assert_eq!(
            repo.find_port(PortProtocol::Tcp, 8025).unwrap(),
            Some(ui.clone())
        );
        assert_eq!(repo.find_port(PortProtocol::Udp, 8025).unwrap(), None);
        assert_eq!(
            repo.reserved_ports(PortProtocol::Tcp).unwrap(),
            BTreeSet::from([1025, 3306, 8025])
        );
        assert_eq!(
            repo.list_owner(OwnerKind::Instance, "b").unwrap(),
            vec![mail.clone(), ui]
        );

        assert_eq!(
            repo.delete_owner_port(&owner("a", "main")).unwrap(),
            Some(mysql.clone())
        );
        assert_eq!(repo.delete_owner_port(&owner("a", "main")).unwrap(), None);
        assert!(!repo.delete(mysql.id).unwrap());
        assert_eq!(repo.delete_owner(OwnerKind::Instance, "b").unwrap(), 2);
        assert!(repo.list().unwrap().is_empty());
    }

    #[test]
    fn duplicates_are_conflicts() {
        let store = Store::open_in_memory().unwrap();
        let repo = PortRepo::new(store.conn());
        repo.insert(&PortReservation::new_tcp(80, None, owner("a", "http")))
            .unwrap();
        let same_port = PortReservation::new_tcp(80, None, owner("b", "http"));
        assert_eq!(repo.insert(&same_port).unwrap_err().code(), "conflict");
        let same_owner = PortReservation::new_tcp(81, None, owner("a", "http"));
        assert_eq!(repo.insert(&same_owner).unwrap_err().code(), "conflict");
        let mut udp = PortReservation::new_tcp(80, None, owner("c", "dns"));
        udp.protocol = PortProtocol::Udp;
        repo.insert(&udp).unwrap();
    }

    #[test]
    fn state_and_conflict_holder_are_updated() {
        let store = Store::open_in_memory().unwrap();
        let repo = PortRepo::new(store.conn());
        let r = PortReservation::new_tcp(3306, None, owner("a", "main"));
        repo.insert(&r).unwrap();
        let holder = PortHolder {
            pid: 4412,
            exe: Some(r"C:\xampp\mysql\bin\mysqld.exe".into()),
        };
        assert!(
            repo.set_state(r.id, PortState::Conflict, Some(&holder))
                .unwrap()
        );
        let stored = repo.get(r.id).unwrap().unwrap();
        assert_eq!(stored.state, PortState::Conflict);
        assert_eq!(stored.conflict, Some(holder));
        assert!(stored.updated_at >= r.updated_at);
        assert!(repo.set_state(r.id, PortState::Bound, None).unwrap());
        let stored = repo.get(r.id).unwrap().unwrap();
        assert_eq!((stored.state, stored.conflict), (PortState::Bound, None));
        assert!(
            !repo
                .set_state(PortReservationId::new(), PortState::Bound, None)
                .unwrap()
        );
    }
}
