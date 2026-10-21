//! 紫垣事件簿：简易 append-only 日志，v1 格式；未来可换成 SQLite。
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use ziyuan_core::{Agent, AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, TaskId};

pub struct EventLog {
    path: PathBuf,
}
impl EventLog {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(".ziyuan").join("court.log"),
        }
    }
    pub fn exists(&self) -> bool {
        self.path.exists()
    }
    pub fn append(&self, events: &[CourtEvent]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut content = String::new();
        for event in events {
            content.push_str(&encode_event(event));
            content.push('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        file.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn load(&self) -> Result<Vec<CourtEvent>, String> {
        let mut data = String::new();
        OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?
            .read_to_string(&mut data)
            .map_err(|e| e.to_string())?;
        data.lines()
            .enumerate()
            .map(|(idx, line)| {
                decode_event(line).map_err(|e| format!("日志第 {} 行: {e}", idx + 1))
            })
            .collect()
    }
}

fn to_hex(value: &str) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(value.len() * 2);
    for b in value.bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}
fn from_hex(s: &str) -> Result<String, String> {
    if s.len() % 2 != 0 {
        return Err("hex 长度不合法".into());
    }
    let mut out = Vec::new();
    for pair in s.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        out.push(u8::from_str_radix(text, 16).map_err(|e| e.to_string())?);
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}
fn encode_event(event: &CourtEvent) -> String {
    use CourtEvent::*;
    match event {
        Founded(mode) => format!("v1\tFOUNDED\t{}", mode.code()),
        AgentRegistered(a) => format!(
            "v1\tREGISTERED\t{}\t{}\t{}",
            a.id.0,
            to_hex(&a.name),
            a.speciality.code()
        ),
        OfficialAppointed { agent, office } => {
            format!("v1\tAPPOINTED\t{}\t{}", agent.0, office.code())
        }
        OfficialDismissed { agent } => format!("v1\tDISMISSED\t{}", agent.0),
        InstitutionReformed(mode) => format!("v1\tREFORMED\t{}", mode.code()),
        EdictIssued(e) => format!(
            "v1\tEDICT\t{}\t{}\t{}",
            e.id.0,
            e.department.code(),
            to_hex(&e.text)
        ),
        TaskCompleted {
            task,
            worker,
            summary,
        } => format!(
            "v1\tCOMPLETED\t{}\t{}\t{}",
            task.0,
            worker.0,
            to_hex(summary)
        ),
    }
}
fn decode_event(line: &str) -> Result<CourtEvent, String> {
    use CourtEvent::*;
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.first() != Some(&"v1") {
        return Err("不支持的事件格式版本".into());
    }
    let field = |i: usize| parts.get(i).copied().ok_or_else(|| format!("缺少字段 {i}"));
    let id =
        |i: usize| -> Result<u64, String> { field(i)?.parse::<u64>().map_err(|e| e.to_string()) };
    let event = match field(1)? {
        "FOUNDED" => Founded(field(2)?.parse::<GovernanceMode>()?),
        "REGISTERED" => AgentRegistered(Agent {
            id: AgentId(id(2)?),
            name: from_hex(field(3)?)?,
            speciality: field(4)?.parse::<Department>()?,
        }),
        "APPOINTED" => OfficialAppointed {
            agent: AgentId(id(2)?),
            office: field(3)?.parse::<Office>()?,
        },
        "DISMISSED" => OfficialDismissed {
            agent: AgentId(id(2)?),
        },
        "REFORMED" => InstitutionReformed(field(2)?.parse::<GovernanceMode>()?),
        "EDICT" => EdictIssued(Edict {
            id: TaskId(id(2)?),
            department: field(3)?.parse::<Department>()?,
            text: from_hex(field(4)?)?,
        }),
        "COMPLETED" => TaskCompleted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            summary: from_hex(field(4)?)?,
        },
        other => return Err(format!("未知事件类型: {other}")),
    };
    let expected_fields = match &event {
        Founded(_) | InstitutionReformed(_) | OfficialDismissed { .. } => 3,
        OfficialAppointed { .. } => 4,
        AgentRegistered(_) | EdictIssued(_) | TaskCompleted { .. } => 5,
    };
    if parts.len() != expected_fields {
        return Err("字段数量不正确".into());
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_round_trip_chinese_and_tabs() {
        let original = CourtEvent::EdictIssued(Edict {
            id: TaskId(42),
            department: Department::Works,
            text: "检查\tRust\n测试".into(),
        });
        assert_eq!(decode_event(&encode_event(&original)).unwrap(), original);
    }
    #[test]
    fn malformed_event_is_rejected() {
        assert!(decode_event("v1\tEDICT\t42\tworks\tbroken").is_err());
    }
}
