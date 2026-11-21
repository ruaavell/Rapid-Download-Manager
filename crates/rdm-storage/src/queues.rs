use crate::Storage;
use rdm_common::{Queue, QueueStatus, Result};
use rusqlite::{params, OptionalExtension, Row};

impl Storage {
    pub fn insert_queue(&self, queue: &Queue) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO queues
                    (id,name,max_concurrent,default_connections,speed_limit,priority,status,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    queue.id,
                    queue.name,
                    queue.max_concurrent as i64,
                    queue.default_connections as i64,
                    queue.speed_limit.map(|v| v as i64),
                    queue.priority,
                    queue.status.as_str(),
                    queue.created_at
                ],
            )?;
            Ok(())
        })
    }

    pub fn update_queue(&self, queue: &Queue) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE queues SET name=?2, max_concurrent=?3, default_connections=?4,
                    speed_limit=?5, priority=?6, status=?7 WHERE id=?1",
                params![
                    queue.id,
                    queue.name,
                    queue.max_concurrent as i64,
                    queue.default_connections as i64,
                    queue.speed_limit.map(|v| v as i64),
                    queue.priority,
                    queue.status.as_str()
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_queue(&self, id: &str) -> Result<Option<Queue>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id,name,max_concurrent,default_connections,speed_limit,priority,status,created_at
                 FROM queues WHERE id=?1",
                [id],
                map_queue,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn list_queues(&self) -> Result<Vec<Queue>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,name,max_concurrent,default_connections,speed_limit,priority,status,created_at
                 FROM queues ORDER BY priority DESC, created_at ASC",
            )?;
            let rows = stmt.query_map([], map_queue)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn delete_queue(&self, id: &str) -> Result<bool> {
        if id == "default" {
            return Ok(false);
        }
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM queues WHERE id=?1", [id])?;
            Ok(changed > 0)
        })
    }

    pub fn set_queue_status(&self, id: &str, status: QueueStatus) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE queues SET status=?2 WHERE id=?1",
                params![id, status.as_str()],
            )?;
            Ok(())
        })
    }

    pub fn queue_task_count(&self, id: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let value: i64 = conn.query_row(
                "SELECT COUNT(*) FROM downloads WHERE queue_id=?1 AND status IN ('queued','paused')",
                [id],
                |row| row.get(0),
            )?;
            Ok(value.max(0) as usize)
        })
    }
}

fn map_queue(row: &Row<'_>) -> rusqlite::Result<Queue> {
    let status: String = row.get("status")?;
    Ok(Queue {
        id: row.get("id")?,
        name: row.get("name")?,
        max_concurrent: row.get::<_, i64>("max_concurrent")?.max(1) as u32,
        default_connections: row.get::<_, i64>("default_connections")?.clamp(1, 32) as u32,
        speed_limit: row
            .get::<_, Option<i64>>("speed_limit")?
            .map(|value| value.max(0) as u64),
        priority: row.get("priority")?,
        status: QueueStatus::parse(&status),
        created_at: row.get("created_at")?,
    })
}
