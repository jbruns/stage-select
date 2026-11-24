//! Games the Operator removed from a Library Folder. Every plan for the folder leaves a removed game out, as if its
//! Selection hadn't selected it, so a Build keeps its files aside, returns it to its Source Set when it is placed there
//! under Single Copy, or, removed to be deleted, keeps its files aside and deletes them. A game joins the folder's
//! removed games in the transaction that approves the plan removing it, and leaves them when the Operator restores it:
//! the next Build puts it back.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use stage_select_api::build::{
    GameRemoval, LibraryFolder, Plan, PlanBuild, PlanKeptDeletion, RemoveAction, RemoveGames,
    RemovedGame, RestoreGames,
};

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

/// Adds the games plan `plan` removes to Library Folder `folder`'s removed games, in the transaction that approves it.
pub(crate) fn record_removals(
    tx: &Transaction,
    folder: i64,
    plan: i64,
    removals: &[GameRemoval],
) -> Result<()> {
    let at = now();
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

    /// Deletes the files plan `plan`'s Build `build` kept aside of the games the plan removed to delete, as an
    /// approved deletion of kept files by `actor`: each only while it still reads as it was just read. A file that
    /// doesn't, or that can't be deleted, stays kept, where you can delete it yourself.
    pub(crate) fn delete_removed(&self, plan: &Plan, build: i64, actor: &str) -> Result<()> {
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
                    .any(|g| *g == r.game || stage_select_api::storage::release_of(g) == r.game)
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
