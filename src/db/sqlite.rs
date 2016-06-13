// This file is released under the same terms as Rust itself.

use db::{Db, PendingEntry, QueueEntry, RunningEntry};
use rusqlite::{self, Connection};
use std::convert::AsRef;
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use ui::Pr;
use vcs::Commit;
use pipeline::PipelineId;

pub struct SqliteDb<C, P>
    where C: Commit + Into<String> + FromStr,
          P: Pr + Into<String> + FromStr
{
    _commit: PhantomData<C>,
    _pr: PhantomData<P>,
    conn: Connection,
}

impl<C, P> SqliteDb<C, P>
    where C: Commit + Into<String> + FromStr,
          P: Pr + Into<String> + FromStr
{
    pub fn open<Q: AsRef<Path>>(path: Q) -> rusqlite::Result<Self> {
        let conn = try!(Connection::open(path));
        try!(conn.execute_batch(r###"
            CREATE TABLE IF NOT EXISTS queue (
                id INTEGER PRIMARY KEY,
                pipeline_id INTEGER,
                pr TEXT,
                message TEXT,
                pull_commit TEXT
            );
            CREATE TABLE IF NOT EXISTS running (
                pipeline_id INTEGER PRIMARY KEY,
                pr TEXT,
                message TEXT,
                pull_commit TEXT,
                merge_commit TEXT,
                canceled INT,
                built INT
            );
            CREATE TABLE IF NOT EXISTS pending (
                id INTEGER PRIMARY KEY,
                pipeline_id INTEGER,
                pr TEXT,
                pull_commit TEXT
            );
        "###));
        Ok(SqliteDb{
            conn: conn,
            _pr: PhantomData,
            _commit: PhantomData,
        })
    }
}

impl<C, P> Db<C, P> for SqliteDb<C, P>
    where C: Commit + Into<String> + FromStr,
          P: Pr + Into<String> + FromStr,
          <C as FromStr>::Err: ::std::error::Error,
          <P as FromStr>::Err: ::std::error::Error,
{
    fn push_queue(
        &mut self,
        pipeline_id: PipelineId,
        QueueEntry{pr, commit, message}: QueueEntry<C, P>
    ) {
        let sql = r###"
            INSERT INTO queue (pr, pipeline_id, pull_commit, message)
            VALUES (?, ?, ?, ?);
        "###;
        self.conn.execute(sql, &[
            &pr.into(),
            &pipeline_id.0,
            &commit.into(),
            &message,
        ]).expect("Push-to-queue");
    }
    fn pop_queue(
        &mut self,
        pipeline_id: PipelineId,
    ) -> Option<QueueEntry<C, P>> {
        let trans = self.conn
            .transaction()
            .expect("Start pop-from-queue transaction");
        let sql = r###"
            SELECT id, pr, pull_commit, message
            FROM queue
            WHERE pipeline_id = ?
            ORDER BY id ASC LIMIT 1;
        "###;
        let item = {
            let mut stmt = trans.prepare(sql).expect("Pop from queue");
            let mut rows = stmt
            .query_map(&[&pipeline_id.0], |row| (
                row.get::<_, i32>(0),
                QueueEntry {
                    pr: P::from_str(&row.get::<_, String>(1)).unwrap(),
                    commit: C::from_str(&row.get::<_, String>(2)).unwrap(),
                    message: row.get::<_, String>(3),
                },
            )).expect("Map pop from queue");
            rows.next().map(|item| item.expect("Retrieve pop from queue"))
        };
        if let Some((id, _)) = item {
            let sql = r###"
                DELETE FROM queue WHERE id = ?;
            "###;
            trans.execute(sql, &[&id]).expect("Delete pop-from-queue row");
        }
        trans.commit().expect("Commit pop-from-queue transaction");
        item.map(|item| item.1)
    }
    fn put_running(
        &mut self,
        pipeline_id: PipelineId,
        RunningEntry{
            pr,
            pull_commit,
            merge_commit,
            message,
            canceled,
            built,
        }: RunningEntry<C, P>
    ) {
        let sql = r###"
            REPLACE INTO running
                (pipeline_id, pr, pull_commit, merge_commit, message, canceled, built)
            VALUES
                (?, ?, ?, ?, ?, ?, ?);
        "###;
        self.conn.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(pr),
            &<C as Into<String>>::into(pull_commit),
            &merge_commit.map(|m| <C as Into<String>>::into(m)),
            &message,
            &canceled,
            &built,
        ]).expect("Put running");
    }
    fn take_running(
        &mut self,
        pipeline_id: PipelineId,
    ) -> Option<RunningEntry<C, P>> {
        let trans = self.conn.transaction()
            .expect("Start take-running transaction");
        let sql = r###"
            SELECT pr, pull_commit, merge_commit, message, canceled, built
            FROM running
            WHERE pipeline_id = ?;
        "###;
        let entry = {
            let mut stmt = trans.prepare(&sql)
                .expect("Prepare take-running query");
            let mut rows = stmt
                .query_map(&[&pipeline_id.0], |row| RunningEntry {
                    pr: P::from_str(&row.get::<_, String>(0)[..]).unwrap(),
                    pull_commit: C::from_str(&row.get::<_, String>(1)[..])
                        .unwrap(),
                    merge_commit: row.get::<_, Option<String>>(2).map(
                        |v| C::from_str(&v).unwrap()
                    ),
                    message: row.get(3),
                    canceled: row.get(4),
                    built: row.get(5),
                }).expect("Get running entry");
            rows.next().map(|item| item.expect("Retrieve running entry"))
        };
        let sql = r###"
            DELETE FROM running WHERE pipeline_id = ?;
        "###;
        trans.execute(sql, &[&pipeline_id.0]).expect("Remove running entry");
        trans.commit().expect("Commit take-running transaction");
        entry
    }
    fn peek_running(
        &mut self,
        pipeline_id: PipelineId,
    ) -> Option<RunningEntry<C, P>> {
        let sql = r###"
            SELECT pr, pull_commit, merge_commit, message, canceled, built
            FROM running
            WHERE pipeline_id = ?;
        "###;
        let mut stmt = self.conn.prepare(&sql)
            .expect("Prepare peek-running query");
        let mut rows = stmt
            .query_map(&[&pipeline_id.0], |row| RunningEntry {
                pr: P::from_str(&row.get::<_, String>(0)[..]).unwrap(),
                pull_commit: C::from_str(&row.get::<_, String>(1)[..])
                    .unwrap(),
                merge_commit: row.get::<_, Option<String>>(2).map(
                    |v| C::from_str(&v).unwrap()
                ),
                message: row.get(3),
                canceled: row.get(4),
                built: row.get(5),
            })
            .expect("Get running entry");
        rows.next()
            .map(|item| item.expect("Retrieve running entry"))
    }
    fn add_pending(
        &mut self,
        pipeline_id: PipelineId,
        entry: PendingEntry<C, P>,
    ) {
        let trans = self.conn.transaction()
            .expect("Start add-pending transaction");
        let sql = r###"
            DELETE FROM pending WHERE pipeline_id = ? AND pr = ?;
        "###;
        trans.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(entry.pr.clone()),
        ]).expect("Remove pending entry");
        let sql = r###"
            INSERT INTO pending (pipeline_id, pr, pull_commit)
            VALUES (?, ?, ?);
        "###;
        trans.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(entry.pr.clone()),
            &<C as Into<String>>::into(entry.commit.clone()),
        ]).expect("Add pending entry");
        trans.commit().expect("Commit add-pending transaction");
    }
    fn take_pending_by_pr(
        &mut self,
        pipeline_id: PipelineId,
        pr: &P,
    ) -> Option<PendingEntry<C, P>> {
        let trans = self.conn.transaction()
            .expect("Start take-pending transaction");
        let sql = r###"
            SELECT id, pr, pull_commit
            FROM pending
            WHERE pipeline_id = ? AND pr = ?;
        "###;
        let entry = {
            let mut stmt = trans.prepare(&sql)
                .expect("Prepare take-pending query");
            let mut rows = stmt
                .query_map(&[
                    &pipeline_id.0,
                    &<P as Into<String>>::into(pr.clone()),
                ], |row| (row.get::<_, i64>(0), PendingEntry {
                    pr: P::from_str(&row.get::<_, String>(1)[..]).unwrap(),
                    commit: C::from_str(&row.get::<_, String>(2)[..])
                        .unwrap(),
                })).expect("Get pending entry");
            rows.next().map(|item| item.expect("Retrieve pending entry"))
        };
        if let Some(ref entry) = entry {
            let sql = r###"
                DELETE FROM pending WHERE id = ?;
            "###;
            trans.execute(sql, &[&entry.0]).expect("Remove pending entry");
            trans.commit().expect("Commit take-pending transaction");
        }
        entry.map(|entry| entry.1)
    }
    fn peek_pending_by_pr(
        &mut self,
        pipeline_id: PipelineId,
        pr: &P,
    ) -> Option<PendingEntry<C, P>> {
        let sql = r###"
            SELECT pr, pull_commit
            FROM pending
            WHERE pipeline_id = ? AND pr = ?;
        "###;
        let mut stmt = self.conn.prepare(&sql)
            .expect("Prepare peek-pending query");
        let mut rows = stmt
            .query_map(&[
                &pipeline_id.0,
                &<P as Into<String>>::into(pr.clone()),
            ], |row| PendingEntry {
                pr: P::from_str(&row.get::<_, String>(0)[..]).unwrap(),
                commit: C::from_str(&row.get::<_, String>(1)[..])
                    .unwrap(),
            })
            .expect("Get pending entry");
        rows.next()
            .map(|item| item.expect("Retrieve pending entry"))
    }
    fn cancel_by_pr(&mut self, pipeline_id: PipelineId, pr: &P) {
        let sql = r###"
            UPDATE running
            SET canceled = 1
            WHERE pipeline_id = ? AND pr = ?
        "###;
        self.conn.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(pr.clone()),
        ]).expect("Cancel running PR");
        let sql = r###"
            DELETE FROM queue
            WHERE pipeline_id = ? AND pr = ?
        "###;
        self.conn.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(pr.clone()),
        ]).expect("Cancel queue entries");
    }
    fn cancel_by_pr_different_commit(
        &mut self,
        pipeline_id: PipelineId,
        pr: &P,
        commit: &C,
    ) -> bool {
        let sql = r###"
            UPDATE running
            SET canceled = 1
            WHERE pipeline_id = ? AND pr = ? AND pull_commit <> ?
        "###;
        let affected_rows_running = self.conn.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(pr.clone()),
            &<C as Into<String>>::into(commit.clone()),
        ]).expect("Cancel running PR");
        let sql = r###"
            DELETE FROM queue
            WHERE pipeline_id = ? AND pr = ? AND pull_commit <> ?
        "###;
        let affected_rows_queue = self.conn.execute(sql, &[
            &pipeline_id.0,
            &<P as Into<String>>::into(pr.clone()),
            &<C as Into<String>>::into(commit.clone()),
        ]).expect("Cancel queue entries");
        affected_rows_queue != 0 || affected_rows_running != 0
    }
}