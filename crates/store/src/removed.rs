//! Games the Operator removed from a Library Folder. Every plan for the folder leaves a removed game out, as if its
//! Selection hadn't selected it, so a Build keeps its files aside, returns it to its Source Set when it is placed there
//! under Single Copy, or, removed to be deleted, keeps its files aside and deletes them. A game joins the folder's
//! removed games in the transaction that approves the plan removing it, and leaves them when the Operator restores it:
//! the next Build puts it back.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use stage_select_api::build::{
    GameRemoval, LibraryFolder, Plan, PlanBuild, PlanKeptDeletion, RemoveAction, RemoveGames,
    RemovedGame, RestoreGames,
};

use stage_select_api::storage::release_of;
use stage_select_engine::dat::{Dat, DumpStatus, Rom};

use crate::folders::folder_by_id;
use crate::{Error, Result, Store, now};

/// The games removed from Library Folder `folder`, by name.
pub(crate) fn removed_games(c: &Connection, folder: i64) -> Result<Vec<RemovedGame>> {
    let mut stmt = c.prepare(
        "SELECT game, removed_at, action, plan_id FROM removed_games WHERE folder_id = ?1 ORDER BY game",
    )?;
    let rows = stmt.query_map([folder], |r| {
        let action: String = r.get(2)?;
        Ok(RemovedGame {
            name: r.get(0)?,
            removed_at: r.get(1)?,
            action: RemoveAction::parse(&action).unwrap_or(RemoveAction::SetAside),
            plan: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What each game removed from Library Folder `folder` was removed with, by name.
pub(crate) fn removed_actions(
    c: &Connection,
    folder: i64,
) -> Result<BTreeMap<String, RemoveAction>> {
    Ok(removed_games(c, folder)?
        .into_iter()
        .map(|g| (g.name, g.action))
        .collect())
}

/// Adds the games plan `plan` removes to Library Folder `folder`'s removed games, in the transaction that approves it
/// by `actor`, and records the deletion of the files of those it removes to delete, to carry out once its Build has
/// kept them aside.
pub(crate) fn record_removals(
    tx: &Transaction,
    folder: i64,
    plan: i64,
    actor: &str,
    removals: &[GameRemoval],
) -> Result<()> {
    let at = now();
    if removals.iter().any(|r| r.action == RemoveAction::Delete) {
        tx.execute(
            "INSERT INTO removal_deletions (plan_id, folder_id, actor) VALUES (?1, ?2, ?3)",
            params![plan, folder, actor],
        )?;
    }
    for removal in removals {
        tx.execute(
            "INSERT INTO removed_games (folder_id, game, action, plan_id, removed_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (folder_id, game) DO UPDATE SET action = ?3, plan_id = ?4, removed_at = ?5",
            params![folder, removal.game, removal.action.as_str(), plan, at],
        )?;
    }
    Ok(())
}

/// Sorts `removals` by game and refuses a game chosen twice, or chosen with removing every built file.
pub(crate) fn normalize(removals: &mut Vec<GameRemoval>, remove_all: bool) -> Result<()> {
    if remove_all && !removals.is_empty() {
        return Err(Error::Refused(
            "you chose both to remove every built file and to remove some games; choose one".into(),
        ));
    }
    removals.sort();
    removals.dedup();
    if let Some(pair) = removals.windows(2).find(|w| w[0].game == w[1].game) {
        return Err(Error::Refused(format!(
            "you chose two things to do with {}; choose one",
            pair[0].game
        )));
    }
    Ok(())
}

/// The other games of `dat` that need ROMs `game`'s placed file holds, by name. That file is self-contained: it holds
/// every ROM `game` lists and every ROM of its BIOS and devices. A game needs one when it lists a ROM of the same
/// content, by SHA-1, or by size and CRC where either has no SHA-1, or when one of its BIOS or devices does. Disks are
/// never placed, always copied, so none counts. A placed file is the only copy of what it holds, and an Audit counts
/// its ROMs for those games too, so deleting it would lose them as well.
pub(crate) fn sharing(dat: &Dat, game: &str) -> Vec<String> {
    let Some(index) = dat.game_index(game) else {
        return Vec::new();
    };
    let counted = |r: &Rom| !r.disk && r.status != DumpStatus::NoDump;
    let held: Vec<&Rom> = dat
        .self_contained(index)
        .into_iter()
        .map(|r| &dat.roms[r])
        .filter(|r| counted(r))
        .collect();
    let same = |a: &Rom, b: &Rom| match (&a.sha1, &b.sha1) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => {
            a.size == b.size
                && matches!((&a.crc, &b.crc), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y))
        }
    };
    let listing: BTreeSet<usize> = dat
        .roms
        .iter()
        .filter(|r| r.game != index && counted(r))
        .filter(|r| held.iter().any(|h| same(h, r)))
        .map(|r| r.game)
        .collect();
    let mut games: BTreeSet<&str> = listing.iter().map(|&g| dat.games[g].as_str()).collect();
    if !listing.is_empty() {
        games.extend(
            (0..dat.games.len())
                .filter(|&g| g != index && !listing.contains(&g))
                .filter(|&g| dat.dependencies(g).iter().any(|d| listing.contains(d)))
                .map(|g| dat.games[g].as_str()),
        );
    }
    games.remove(game);
    games.into_iter().map(str::to_owned).collect()
}

/// Why deleting `game`, placed here as its source set's only copy, is refused when `sharing` are the other games that
/// need its ROMs, or none when nothing does; `None` for `sharing` when that can't be read.
pub(crate) fn shared_refusal(game: &str, sharing: Option<&[String]>) -> Option<String> {
    let send_back = "sending it back to your source set still works";
    match sharing {
        None => Some(format!(
            "{game} is placed here as its source set's only copy, and its DAT can't be read to tell whether other \
             games need its ROMs, so it can't be deleted; {send_back}"
        )),
        Some([]) => None,
        Some(games) => {
            let named = match games {
                [one] => one.clone(),
                [a, b] => format!("{a} and {b}"),
                [a, b, c] => format!("{a}, {b} and {c}"),
                [a, b, c, rest @ ..] => format!("{a}, {b}, {c} and {} more", rest.len()),
                [] => unreachable!(),
            };
            Some(format!(
                "{game} is placed here as its source set's only copy, and {named} also need its ROMs, so deleting \
                 it would lose them too; {send_back}"
            ))
        }
    }
}

/// `removals`, each of a multi-disc release's name that isn't itself one of `titles` replaced by its discs among them.
pub(crate) fn expand_releases(
    removals: Vec<GameRemoval>,
    titles: &BTreeSet<String>,
) -> Vec<GameRemoval> {
    removals
        .into_iter()
        .flat_map(|removal| {
            let discs: Vec<GameRemoval> = match titles.contains(&removal.game) {
                true => Vec::new(),
                false => titles
                    .iter()
                    .filter(|t| release_of(t) == removal.game && **t != removal.game)
                    .map(|t| GameRemoval {
                        game: t.clone(),
                        action: removal.action,
                    })
                    .collect(),
            };
            match discs.is_empty() {
                true => vec![removal],
                false => discs,
            }
        })
        .collect()
}

/// Why `removal` can't be planned for a game the folder holds `built` files of, `placed` there as its Source Set's
/// only copy when true; none when it can.
pub(crate) fn refusal(removal: &GameRemoval, built: bool, placed: bool) -> Option<String> {
    let game = &removal.game;
    match removal.action {
        _ if !built && !placed => Some(format!(
            "{game} has no file built in this play library folder, so there is nothing to remove"
        )),
        RemoveAction::SendBack if !placed => Some(format!(
            "{game} isn't placed here from its source set, so it can't be sent back; set it aside or delete it"
        )),
        RemoveAction::SetAside if placed => Some(format!(
            "{game} is placed here as its source set's only copy, so it can't be set aside; send it back to its \
             source set, or delete it"
        )),
        _ => None,
    }
}

impl Store {
    /// Plans removing `request.games` from Library Folder `folder` with `request.action`, from the Selection of its
    /// current Build, and stores the plan for review. The games join the folder's removed games once you approve it.
    pub fn plan_game_removal(&self, folder: i64, request: &RemoveGames) -> Result<Plan> {
        if request.games.is_empty() {
            return Err(Error::Refused("pick at least one game to remove".into()));
        }
        let selection: Option<i64> = self.read(|c| {
            folder_by_id(c, folder)?.check_registered()?;
            Ok(c.query_row(
                "SELECT b.selection_id FROM library_folders f JOIN builds b ON b.id = f.current_build_id
                 WHERE f.id = ?1",
                [folder],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        let selection = selection.ok_or_else(|| {
            Error::Refused(
                "nothing was built in this play library folder, so there is nothing to remove"
                    .into(),
            )
        })?;
        let mut plan = PlanBuild::new(selection);
        plan.choices.remove = request
            .games
            .iter()
            .map(|game| GameRemoval {
                game: game.clone(),
                action: request.action,
            })
            .collect();
        self.plan_build_with(folder, &plan)
    }

    /// Carries out the deletion plan `plan`'s approval recorded, of the files its Build `build` kept aside of the
    /// games it removed to delete, unless it is finished: an approved deletion of kept files by whoever approved the
    /// plan, of each only while it is still the file that was kept and reads as it was. A file that doesn't, or that
    /// can't be deleted, stays kept, where you can delete it yourself. Left unfinished, to try again when the service
    /// next starts, while another change to the folder's kept files is under way.
    pub(crate) fn delete_removed(&self, plan: &Plan, build: i64) -> Result<()> {
        let pending: Option<String> = self.read(|c| {
            Ok(c.query_row(
                "SELECT actor FROM removal_deletions WHERE plan_id = ?1 AND finished_at IS NULL",
                [plan.id],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        let Some(actor) = pending else {
            return Ok(());
        };
        match self.delete_kept_removals(plan, build, &actor) {
            Ok(()) | Err(Error::Refused(_)) => self.finish_removal_deletion(plan.id),
            Err(err) => Err(err),
        }
    }

    fn finish_removal_deletion(&self, plan: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE removal_deletions SET finished_at = ?2 WHERE plan_id = ?1",
                params![plan, now()],
            )?;
            Ok(())
        })
    }

    /// Run when the service starts, after [`Store::recover_kept_deletions`]: carries out each deletion of removed
    /// games' files the service stopped before carrying out, once its plan's Build succeeded; one whose Build
    /// didn't, which kept nothing to delete, is finished as it is, and one whose Build is still to run is left to it.
    /// Returns, for each left unfinished, its plan and why.
    pub fn recover_removal_deletions(&self) -> Result<Vec<(i64, String)>> {
        let pending: Vec<(i64, Option<i64>, Option<String>)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT d.plan_id, b.id, j.state FROM removal_deletions d
                 LEFT JOIN jobs j ON j.plan_id = d.plan_id AND j.kind = 'build'
                 LEFT JOIN builds b ON b.job_id = j.id AND b.state = 'succeeded'
                 WHERE d.finished_at IS NULL ORDER BY d.plan_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        let mut unfinished = Vec::new();
        for (plan, build, job) in pending {
            match (build, job.as_deref()) {
                (Some(build), _) => match self.delete_removed(&self.plan(plan)?, build) {
                    Ok(()) => {}
                    Err(Error::Conflict(why)) => unfinished.push((plan, why)),
                    Err(err) => return Err(err),
                },
                (None, Some("queued" | "running")) => {}
                (None, _) => self.finish_removal_deletion(plan)?,
            }
        }
        Ok(unfinished)
    }

    /// Approves deleting the files plan `plan`'s Build `build` kept aside of the games it removed to delete, by
    /// `actor`.
    fn delete_kept_removals(&self, plan: &Plan, build: i64, actor: &str) -> Result<()> {
        let content = &plan.content;
        let deleted: Vec<&str> = content
            .choices
            .remove
            .iter()
            .filter(|r| r.action == RemoveAction::Delete)
            .map(|r| r.game.as_str())
            .collect();
        if deleted.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = content
            .removals
            .iter()
            .filter(|r| r.keep.is_some())
            .filter(|r| {
                deleted
                    .iter()
                    .any(|g| *g == r.game || release_of(g) == r.game)
            })
            .map(|r| r.target.as_str())
            .collect();
        let ids: Vec<i64> = self
            .kept_files(content.folder)?
            .into_iter()
            .filter(|k| k.build == build && !k.directory && names.contains(&k.name.as_str()))
            .map(|k| k.id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let deletion =
            self.plan_kept_deletion(content.folder, &PlanKeptDeletion { ids, all: false })?;
        self.approve_kept_deletion(deletion.id, content.folder, &deletion.digest, actor)?;
        Ok(())
    }

    /// Restores `request.games`, removed from Library Folder `folder`: the next Build puts them back. Refused for a
    /// game that isn't removed.
    pub fn restore_games(&self, folder: i64, request: &RestoreGames) -> Result<LibraryFolder> {
        if request.games.is_empty() {
            return Err(Error::Refused("pick at least one game to restore".into()));
        }
        self.write(|tx| {
            folder_by_id(tx, folder)?.check_registered()?;
            for game in &request.games {
                let gone = tx.execute(
                    "DELETE FROM removed_games WHERE folder_id = ?1 AND game = ?2",
                    params![folder, game],
                )?;
                if gone == 0 {
                    return Err(Error::Refused(format!(
                        "{game} isn't removed from this play library folder"
                    )));
                }
            }
            Ok(())
        })?;
        self.library_folder(folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removal(game: &str, action: RemoveAction) -> GameRemoval {
        GameRemoval {
            game: game.into(),
            action,
        }
    }

    #[test]
    fn sending_back_is_only_for_a_placed_game_and_setting_aside_never_is() {
        let send = removal("A", RemoveAction::SendBack);
        let aside = removal("A", RemoveAction::SetAside);
        let delete = removal("A", RemoveAction::Delete);
        assert!(
            refusal(&send, true, false)
                .unwrap()
                .contains("isn't placed here")
        );
        assert_eq!(refusal(&send, true, true), None);
        assert!(refusal(&aside, true, true).unwrap().contains("only copy"));
        assert_eq!(refusal(&aside, true, false), None);
        assert_eq!(refusal(&delete, true, true), None);
        assert_eq!(refusal(&delete, true, false), None);
        assert!(
            refusal(&delete, false, false)
                .unwrap()
                .contains("no file built")
        );
    }

    #[test]
    fn a_placed_game_whose_roms_other_games_need_is_never_deleted() {
        let xml = br#"<?xml version="1.0"?>
<datafile>
  <header><name>Arcade</name><description>Arcade</description><version>1</version></header>
  <game name="parent"><rom name="a.bin" size="4" crc="11111111"/><rom name="b.bin" size="4" crc="22222222"/></game>
  <game name="clone" cloneof="parent" romof="parent"><rom name="a.bin" merge="a.bin" size="4" crc="11111111"/><rom name="c.bin" size="4" crc="33333333"/></game>
  <game name="cousin" cloneof="parent" romof="parent"><rom name="g.bin" size="4" crc="55555555"/></game>
  <game name="sharer"><rom name="x.bin" size="4" crc="22222222"/></game>
  <game name="alone"><rom name="d.bin" size="4" crc="44444444"/><rom name="e.bin" size="0" status="nodump"/></game>
  <game name="nodumps"><rom name="f.bin" size="0" status="nodump"/></game>
</datafile>"#;
        let dat = Dat::parse(xml, std::path::Path::new("arcade.dat")).unwrap();
        // A clone that lists none of its parent's ROMs doesn't need them.
        assert_eq!(sharing(&dat, "parent"), ["clone", "sharer"]);
        assert_eq!(sharing(&dat, "sharer"), ["parent"]);
        assert!(sharing(&dat, "alone").is_empty());
        assert_eq!(sharing(&dat, "clone"), ["parent"]);

        let named = shared_refusal("parent", Some(&sharing(&dat, "parent"))).unwrap();
        assert!(
            named.contains("clone and sharer also need its ROMs"),
            "{named}"
        );
        assert!(
            named.contains("sending it back to your source set still works"),
            "{named}"
        );
        assert_eq!(shared_refusal("alone", Some(&[])), None);
        assert!(
            shared_refusal("alone", None)
                .unwrap()
                .contains("can't be deleted")
        );
        let many: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).into();
        assert!(
            shared_refusal("p", Some(&many))
                .unwrap()
                .contains("a, b, c and 2 more")
        );
    }

    #[test]
    fn a_placed_file_holds_its_bios_and_devices_roms_but_never_a_disk() {
        let xml = br#"<?xml version="1.0"?>
<mame build="0.289 (mame0289)">
  <machine name="sndchip" isdevice="yes" runnable="no">
    <description>Sound chip</description>
    <rom name="snd.u1" size="8" crc="50a00001" sha1="50a0000000000000000000000000000000000001"/>
  </machine>
  <machine name="solo">
    <description>Solo</description>
    <rom name="solo.u1" size="8" crc="50100001" sha1="5010000000000000000000000000000000000001"/>
    <device_ref name="sndchip"/>
  </machine>
  <machine name="other">
    <description>Other</description>
    <rom name="other.u1" size="8" crc="07e00001" sha1="07e0000000000000000000000000000000000001"/>
    <device_ref name="sndchip"/>
  </machine>
  <machine name="disked">
    <description>Disked</description>
    <rom name="disked.u1" size="8" crc="d1500001" sha1="d150000000000000000000000000000000000001"/>
    <disk name="shared" sha1="d15c000000000000000000000000000000000001"/>
  </machine>
  <machine name="diskmate">
    <description>Diskmate</description>
    <rom name="diskmate.u1" size="8" crc="d1500002" sha1="d150000000000000000000000000000000000002"/>
    <disk name="shared" sha1="d15c000000000000000000000000000000000001"/>
  </machine>
</mame>"#;
        let dat = Dat::parse(xml, std::path::Path::new("mame.xml")).unwrap();
        // Solo's file holds its sound chip's ROM, the only copy of what Other needs.
        assert_eq!(sharing(&dat, "solo"), ["other", "sndchip"]);
        // The disk they both list is copied, never placed.
        assert!(sharing(&dat, "disked").is_empty());
    }

    #[test]
    fn a_release_name_stands_for_its_discs_unless_it_is_a_game_itself() {
        let titles: BTreeSet<String> = [
            "Saga (USA) (Disc 1)",
            "Saga (USA) (Disc 2)",
            "Orbit (USA)",
            "Orbit (USA) (Disc 1)",
        ]
        .map(String::from)
        .into();
        let expanded = expand_releases(
            vec![
                removal("Saga (USA)", RemoveAction::Delete),
                removal("Orbit (USA)", RemoveAction::SetAside),
                removal("Nothing (USA)", RemoveAction::SetAside),
            ],
            &titles,
        );
        assert_eq!(
            expanded,
            [
                removal("Saga (USA) (Disc 1)", RemoveAction::Delete),
                removal("Saga (USA) (Disc 2)", RemoveAction::Delete),
                removal("Orbit (USA)", RemoveAction::SetAside),
                removal("Nothing (USA)", RemoveAction::SetAside),
            ]
        );
    }

    #[test]
    fn a_game_chosen_twice_or_with_removing_everything_is_refused() {
        let mut same = vec![
            removal("B", RemoveAction::Delete),
            removal("B", RemoveAction::Delete),
        ];
        normalize(&mut same, false).unwrap();
        assert_eq!(same.len(), 1);
        let mut two = vec![
            removal("B", RemoveAction::Delete),
            removal("B", RemoveAction::SetAside),
        ];
        assert!(normalize(&mut two, false).is_err());
        let mut all = vec![removal("B", RemoveAction::Delete)];
        assert!(normalize(&mut all, true).is_err());
    }
}
