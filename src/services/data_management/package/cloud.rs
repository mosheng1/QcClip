use super::{Result, invalid};
use crate::services::sync::webdav::Client;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct CloudPackage {
    pub name: String,
    pub created_at: i64,
    pub bytes: u64,
}
impl CloudPackage {
    fn parse(name: String, bytes: u64) -> Option<Self> {
        let stem = name.strip_prefix("qcclip-")?.strip_suffix(".qcclip-data")?;
        let (time, id) = stem.split_once('-')?;
        let created_at = time.parse::<i64>().ok().filter(|t| *t > 0)?;
        if uuid::Uuid::parse_str(id).ok()?.to_string() != id {
            return None;
        }
        Some(Self {
            name,
            created_at,
            bytes,
        })
    }
    pub fn list(client: &Client) -> Result<Vec<Self>> {
        let mut packages: Vec<_> = client
            .list_package_files()?
            .into_iter()
            .filter_map(|(name, size)| Self::parse(name, size))
            .collect();
        packages.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(packages)
    }
    pub fn upload(client: &Client, path: &Path) -> Result<()> {
        client.mkdir("")?;
        client.mkdir("packages/")?;
        let name = format!(
            "qcclip-{}-{}.qcclip-data",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        );
        // 每个包有独立随机名称，无需覆盖共享索引，其他设备可同时上传。
        client.put_file(&format!("packages/{name}"), path)?;
        Ok(())
    }
    pub fn download(&self, client: &Client, path: &Path) -> Result<()> {
        if Self::parse(self.name.clone(), self.bytes).is_none() {
            return invalid("云端数据包名称无效");
        }
        client.get_file(
            &format!("packages/{}", self.name),
            path,
            64 * 1024 * 1024 * 1024,
        )?;
        Ok(())
    }
    pub fn delete(&self, client: &Client) -> Result<()> {
        if Self::parse(self.name.clone(), self.bytes).is_none() {
            return invalid("云端数据包名称无效");
        }
        client.delete(&format!("packages/{}", self.name))?;
        Ok(())
    }
}
