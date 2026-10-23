//! 事件簿 v1 文本编码：每行一个事件，字段以 Tab 分隔，自由文本以 hex 编码。
use ziyuan_core::{Agent, AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, TaskId};

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
    if !s.len().is_multiple_of(2) {
        return Err("hex 长度不合法".into());
    }
    let mut out = Vec::new();
    for pair in s.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        out.push(u8::from_str_radix(text, 16).map_err(|e| e.to_string())?);
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}
pub(crate) fn encode_event(event: &CourtEvent) -> String {
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
        TaskAssigned { task, worker } => format!("v1\tASSIGNED\t{}\t{}", task.0, worker.0),
        ToolInvoked {
            task,
            tool,
            args_json,
            ok,
        } => format!(
            "v1\tTOOL\t{}\t{}\t{}\t{}",
            task.0,
            to_hex(tool),
            to_hex(args_json),
            u8::from(*ok)
        ),
        TaskFailed { task, reason } => format!("v1\tFAILED\t{}\t{}", task.0, to_hex(reason)),
        TaskCancelled { task } => format!("v1\tCANCELLED\t{}", task.0),
    }
}
pub(crate) fn decode_event(line: &str) -> Result<CourtEvent, String> {
    use CourtEvent::*;
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.first() != Some(&"v1") {
        return Err("不支持的事件格式版本".into());
    }
    let field = |i: usize| parts.get(i).copied().ok_or_else(|| format!("缺少字段 {i}"));
    let id =
        |i: usize| -> Result<u64, String> { field(i)?.parse::<u64>().map_err(|e| e.to_string()) };
    let event = match field(1)? {
        "FOUNDED" => Founded(
            field(2)?
                .parse::<GovernanceMode>()
                .map_err(|e| e.to_string())?,
        ),
        "REGISTERED" => AgentRegistered(Agent {
            id: AgentId(id(2)?),
            name: from_hex(field(3)?)?,
            speciality: field(4)?.parse::<Department>().map_err(|e| e.to_string())?,
        }),
        "APPOINTED" => OfficialAppointed {
            agent: AgentId(id(2)?),
            office: field(3)?.parse::<Office>().map_err(|e| e.to_string())?,
        },
        "DISMISSED" => OfficialDismissed {
            agent: AgentId(id(2)?),
        },
        "REFORMED" => InstitutionReformed(
            field(2)?
                .parse::<GovernanceMode>()
                .map_err(|e| e.to_string())?,
        ),
        "EDICT" => EdictIssued(Edict {
            id: TaskId(id(2)?),
            department: field(3)?.parse::<Department>().map_err(|e| e.to_string())?,
            text: from_hex(field(4)?)?,
        }),
        "COMPLETED" => TaskCompleted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            summary: from_hex(field(4)?)?,
        },
        "ASSIGNED" => TaskAssigned {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
        },
        "TOOL" => ToolInvoked {
            task: TaskId(id(2)?),
            tool: from_hex(field(3)?)?,
            args_json: from_hex(field(4)?)?,
            ok: match field(5)? {
                "1" => true,
                "0" => false,
                other => return Err(format!("成败标记无效: {other}")),
            },
        },
        "FAILED" => TaskFailed {
            task: TaskId(id(2)?),
            reason: from_hex(field(3)?)?,
        },
        "CANCELLED" => TaskCancelled {
            task: TaskId(id(2)?),
        },
        other => return Err(format!("未知事件类型: {other}")),
    };
    let expected_fields = match &event {
        Founded(_) | InstitutionReformed(_) | OfficialDismissed { .. } | TaskCancelled { .. } => 3,
        OfficialAppointed { .. } | TaskAssigned { .. } | TaskFailed { .. } => 4,
        AgentRegistered(_) | EdictIssued(_) | TaskCompleted { .. } => 5,
        ToolInvoked { .. } => 6,
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
        let task = TaskId(42);
        let events = [
            CourtEvent::EdictIssued(Edict {
                id: task,
                department: Department::Works,
                text: "检查\tRust\n测试".into(),
            }),
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(7),
            },
            CourtEvent::ToolInvoked {
                task,
                tool: "read_file".into(),
                args_json: r#"{"path":"src/\t主.rs"}"#.into(),
                ok: false,
            },
            CourtEvent::TaskFailed {
                task,
                reason: "超时\n（300s）".into(),
            },
            CourtEvent::TaskCancelled { task },
        ];
        for original in events {
            assert_eq!(decode_event(&encode_event(&original)).unwrap(), original);
        }
    }
    #[test]
    fn malformed_event_is_rejected() {
        assert!(decode_event("v1\tEDICT\t42\tworks\tbroken").is_err());
    }
}
