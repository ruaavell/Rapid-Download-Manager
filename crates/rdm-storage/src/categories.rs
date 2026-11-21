use crate::Storage;
use rdm_common::{Category, RdmError, Result};
use rusqlite::{params, OptionalExtension, Row};

impl Storage {
    pub fn insert_category(&self, category: &Category) -> Result<()> {
        let extensions = serde_json::to_string(&category.extensions)
            .map_err(|err| RdmError::other(format!("invalid category extensions: {err}")))?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO download_categories (id,name,extensions,directory) VALUES (?1,?2,?3,?4)",
                params![category.id, category.name, extensions, category.directory],
            )?;
            Ok(())
        })
    }

    pub fn update_category(&self, category: &Category) -> Result<()> {
        let extensions = serde_json::to_string(&category.extensions)
            .map_err(|err| RdmError::other(format!("invalid category extensions: {err}")))?;
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE download_categories SET name=?2, extensions=?3, directory=?4 WHERE id=?1",
                params![category.id, category.name, extensions, category.directory],
            )?;
            Ok(())
        })
    }

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,name,extensions,directory FROM download_categories ORDER BY name ASC",
            )?;
            let rows = stmt.query_map([], map_category)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    pub fn get_category(&self, id: &str) -> Result<Option<Category>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id,name,extensions,directory FROM download_categories WHERE id=?1",
                [id],
                map_category,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn delete_category(&self, id: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM download_categories WHERE id=?1", [id])?;
            Ok(changed > 0)
        })
    }

    pub fn category_for_filename(&self, filename: &str) -> Result<Option<Category>> {
        let extension = filename
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        Ok(self.list_categories()?.into_iter().find(|category| {
            category
                .extensions
                .iter()
                .any(|ext| ext.eq_ignore_ascii_case(&extension))
        }))
    }
}

fn map_category(row: &Row<'_>) -> rusqlite::Result<Category> {
    let extensions: String = row.get("extensions")?;
    Ok(Category {
        id: row.get("id")?,
        name: row.get("name")?,
        extensions: serde_json::from_str(&extensions).unwrap_or_default(),
        directory: row.get("directory")?,
    })
}
