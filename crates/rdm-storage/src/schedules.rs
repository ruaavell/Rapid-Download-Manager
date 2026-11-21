use crate::Storage;
use rdm_common::{RepeatRule, Result, Schedule, ScheduleAction};
use rusqlite::{params, OptionalExtension, Row};

impl Storage {
    pub fn insert_schedule(&self, schedule: &Schedule) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO scheduled_downloads
                    (id,name,action,target_id,run_at,repeat_rule,enabled,confirmed,last_run,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    schedule.id,
                    schedule.name,
                    action_str(schedule.action),
                    schedule.target_id,
                    schedule.run_at,
                    repeat_str(schedule.repeat_rule),
                    schedule.enabled as i64,
                    schedule.confirmed as i64,
                    schedule.last_run,
                    schedule.created_at
                ],
            )?;
            Ok(())
        })
    }

    pub fn update_schedule(&self, schedule: &Schedule) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE scheduled_downloads SET name=?2, action=?3, target_id=?4, run_at=?5,
                    repeat_rule=?6, enabled=?7, confirmed=?8, last_run=?9 WHERE id=?1",
                params![
                    schedule.id,
                    schedule.name,
                    action_str(schedule.action),
                    schedule.target_id,
                    schedule.run_at,
                    repeat_str(schedule.repeat_rule),
                    schedule.enabled as i64,
                    schedule.confirmed as i64,
                    schedule.last_run
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,name,action,target_id,run_at,repeat_rule,enabled,confirmed,last_run,created_at
                 FROM scheduled_downloads ORDER BY run_at ASC",
            )?;
            let rows = stmt.query_map([], map_schedule)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn due_schedules(&self, now: i64) -> Result<Vec<Schedule>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,name,action,target_id,run_at,repeat_rule,enabled,confirmed,last_run,created_at
                 FROM scheduled_downloads WHERE enabled=1 AND run_at<=?1 ORDER BY run_at ASC",
            )?;
            let rows = stmt.query_map([now], map_schedule)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn delete_schedule(&self, id: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM scheduled_downloads WHERE id=?1", [id])?;
            Ok(changed > 0)
        })
    }

    pub fn get_schedule(&self, id: &str) -> Result<Option<Schedule>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id,name,action,target_id,run_at,repeat_rule,enabled,confirmed,last_run,created_at
                 FROM scheduled_downloads WHERE id=?1",
                [id],
                map_schedule,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn mark_schedule_run(&self, id: &str, last_run: i64, next_run: Option<i64>) -> Result<()> {
        self.with_conn(|conn| match next_run {
            Some(next) => {
                conn.execute(
                    "UPDATE scheduled_downloads SET last_run=?2, run_at=?3 WHERE id=?1",
                    params![id, last_run, next],
                )?;
                Ok(())
            }
            None => {
                conn.execute(
                    "UPDATE scheduled_downloads SET last_run=?2, enabled=0 WHERE id=?1",
                    params![id, last_run],
                )?;
                Ok(())
            }
        })
    }
}

fn map_schedule(row: &Row<'_>) -> rusqlite::Result<Schedule> {
    let action: String = row.get("action")?;
    let repeat: String = row.get("repeat_rule")?;
    Ok(Schedule {
        id: row.get("id")?,
        name: row.get("name")?,
        action: parse_action(&action),
        target_id: row.get("target_id")?,
        run_at: row.get("run_at")?,
        repeat_rule: parse_repeat(&repeat),
        enabled: row.get::<_, i64>("enabled")? != 0,
        confirmed: row.get::<_, i64>("confirmed")? != 0,
        last_run: row.get("last_run")?,
        created_at: row.get("created_at")?,
    })
}

fn action_str(action: ScheduleAction) -> &'static str {
    match action {
        ScheduleAction::StartQueue => "start_queue",
        ScheduleAction::PauseQueue => "pause_queue",
        ScheduleAction::ResumeAll => "resume_all",
        ScheduleAction::PauseAll => "pause_all",
        ScheduleAction::SetGlobalSpeedLimit => "set_global_speed_limit",
        ScheduleAction::Shutdown => "shutdown",
        ScheduleAction::Suspend => "suspend",
    }
}

fn parse_action(value: &str) -> ScheduleAction {
    match value {
        "pause_queue" => ScheduleAction::PauseQueue,
        "resume_all" => ScheduleAction::ResumeAll,
        "pause_all" => ScheduleAction::PauseAll,
        "set_global_speed_limit" => ScheduleAction::SetGlobalSpeedLimit,
        "shutdown" => ScheduleAction::Shutdown,
        "suspend" => ScheduleAction::Suspend,
        _ => ScheduleAction::StartQueue,
    }
}

fn repeat_str(rule: RepeatRule) -> &'static str {
    match rule {
        RepeatRule::Once => "once",
        RepeatRule::Hourly => "hourly",
        RepeatRule::Daily => "daily",
        RepeatRule::Weekly => "weekly",
    }
}

fn parse_repeat(value: &str) -> RepeatRule {
    match value {
        "hourly" => RepeatRule::Hourly,
        "daily" => RepeatRule::Daily,
        "weekly" => RepeatRule::Weekly,
        _ => RepeatRule::Once,
    }
}
