//! 事件簿 v1 文本编码：每行一个事件，字段以 Tab 分隔，自由文本以 hex 编码。
use ziyuan_core::{
    Agent, AgentId, BudgetScope, Capability, CensorVerdict, CourtEvent, Department, Edict,
    Evidence, FileChange, GovernanceMode, Nomination, Office, Seal, Speech, Subtask, TaskId,
    Template, Threat, TokenUsage, Verdict,
};

fn to_hex(value: &str) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(value.len() * 2);
    for b in value.bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}
/// 十六进制字符的值；非法字符为 0xFF。
const NIBBLES: [u8; 256] = {
    let mut table = [0xFF; 256];
    let mut i = 0;
    while i < 10 {
        table[b'0' as usize + i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < 6 {
        table[b'a' as usize + i] = 10 + i as u8;
        table[b'A' as usize + i] = 10 + i as u8;
        i += 1;
    }
    table
};

fn from_hex(s: &str) -> Result<String, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex 长度不合法".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut invalid = 0;
    for [a, b] in s.as_bytes().as_chunks::<2>().0 {
        let (hi, lo) = (NIBBLES[*a as usize], NIBBLES[*b as usize]);
        invalid |= hi | lo;
        out.push(hi << 4 | lo);
    }
    // 合法值都小于 16，任一非法字符都会让高位置 1。
    if invalid > 0x0F {
        return Err("hex 含非法字符".into());
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
        OfficialPurged { agent } => format!("v1\tPURGED\t{}", agent.0),
        EdictApproved { task } => format!("v1\tAPPROVED\t{}", task.0),
        EdictDrafted {
            task,
            worker,
            note,
            model,
            usage,
        } => {
            let line = format!(
                "v1\tDRAFTED\t{}\t{}\t{}\t{}\t{}",
                task.0,
                worker.0,
                to_hex(note),
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        InstitutionReformed(mode) => format!("v1\tREFORMED\t{}", mode.code()),
        EmperorRetreat(on) => format!("v1\tRETREAT\t{}", u8::from(*on)),
        EdictIssued(e) => format!(
            "v1\tEDICT\t{}\t{}\t{}\t{}",
            e.id.0,
            e.department.code(),
            to_hex(&e.text),
            encode_seal(&e.seal)
        ),
        OfficeModelSet { office, model } => match model {
            Some(model) => format!("v1\tMODEL\t{}\t{}", office.code(), to_hex(model)),
            None => format!("v1\tMODEL\t{}", office.code()),
        },
        // 范围为 `court`、`office:<官职>` 或 `task:<ID>`；取消度支时没有额度字段。
        BudgetSet { scope, tokens } => {
            let scope = match scope {
                BudgetScope::Court => "court".to_owned(),
                BudgetScope::Office(office) => format!("office:{}", office.code()),
                BudgetScope::Task(task) => format!("task:{}", task.0),
            };
            match tokens {
                Some(t) => format!("v1\tBUDGET\t{scope}\t{t}"),
                None => format!("v1\tBUDGET\t{scope}"),
            }
        }
        OfficeCeilingSet { office, ceiling } => match ceiling {
            Some(seal) => format!("v1\tCEILING\t{}\t{}", office.code(), encode_seal(seal)),
            None => format!("v1\tCEILING\t{}", office.code()),
        },
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
        TaskAssigned {
            task,
            worker,
            model: None,
        } => format!("v1\tASSIGNED\t{}\t{}", task.0, worker.0),
        TaskAssigned {
            task,
            worker,
            model: Some(model),
        } => format!("v1\tASSIGNED\t{}\t{}\t{}", task.0, worker.0, to_hex(model)),
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
        // 每处改动为 `路径:原文:新文`（均 hex，新建文件的原文为 `-`），以逗号分隔。
        ChangesProposed { task, changes } => {
            let entries: Vec<_> = changes
                .iter()
                .map(|c| {
                    let before = c.before.as_deref().map_or("-".into(), to_hex);
                    format!("{}:{before}:{}", to_hex(&c.path), to_hex(&c.after))
                })
                .collect();
            format!("v1\tCHANGES\t{}\t{}", task.0, entries.join(","))
        }
        // 逐段准奏时附驳回的段号（逗号分隔）；全部准奏不附，与 v0.3 一致。
        ChangesApplied { task, skipped } if skipped.is_empty() => {
            format!("v1\tAPPLIED\t{}", task.0)
        }
        ChangesApplied { task, skipped } => {
            let skipped: Vec<_> = skipped.iter().map(u32::to_string).collect();
            format!("v1\tAPPLIED\t{}\t{}", task.0, skipped.join(","))
        }
        // 子诏令为 `ID:承办者:内容hex`，以逗号分隔。
        EdictSplit {
            task,
            subtasks,
            note,
            model,
            usage,
        } => {
            let parts: Vec<_> = subtasks
                .iter()
                .map(|s| format!("{}:{}:{}", s.id.0, s.worker.0, to_hex(&s.text)))
                .collect();
            let line = format!(
                "v1\tSPLIT\t{}\t{}\t{}\t{}\t{}",
                task.0,
                parts.join(","),
                to_hex(note),
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        // 发言为 `官员ID;官职;内容hex;输入;输出;模型hex（无模型为空）`，以逗号分隔。
        CourtDebated { task, speeches } => {
            format!("v1\tDEBATED\t{}\t{}", task.0, encode_speeches(speeches))
        }
        ExamTaken {
            agent,
            exam,
            score,
            answer,
            model,
            usage,
        } => {
            let line = format!(
                "v1\tEXAM\t{}\t{}\t{score}\t{}\t{}\t{}",
                agent.0,
                to_hex(exam),
                usage.input,
                usage.output,
                to_hex(answer)
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        PalaceExamHeld { question, answers } => format!(
            "v1\tPALACE\t{}\t{}",
            to_hex(question),
            encode_speeches(answers)
        ),
        ZhuangyuanNamed { agent } => format!("v1\tZHUANGYUAN\t{}", agent.0),
        CourierDelivered { task, station } => {
            format!("v1\tCOURIER\t{}\t{}", task.0, to_hex(station))
        }
        EdictExpedited { task } => format!("v1\tURGENT\t{}", task.0),
        CourierReplied {
            task,
            pull_request,
            url,
        } => format!(
            "v1\tREPLIED\t{}\t{}\t{}",
            task.0,
            if *pull_request { "pr" } else { "comment" },
            to_hex(url)
        ),
        // 验收标准各条单行，以换行连接后整体 hex。
        TemplateSaved { name, template } => format!(
            "v1\tTEMPLATE\t{}\t{}\t{}\t{}\t{}",
            to_hex(name),
            template.department.code(),
            to_hex(&template.text),
            encode_seal(&template.seal),
            to_hex(&template.criteria.join("\n"))
        ),
        TemplateRemoved { name } => format!("v1\tUNTEMPLATE\t{}", to_hex(name)),
        CourtNominated { office, votes } => {
            format!("v1\tNOMINATED\t{}\t{}", office.code(), encode_votes(votes))
        }
        OfficialCaned { agent } => format!("v1\tCANED\t{}", agent.0),
        OfficialPardoned { agent } => format!("v1\tPARDONED\t{}", agent.0),
        TemplateApplied {
            task,
            name,
            criteria,
        } => format!(
            "v1\tTEMPLATED\t{}\t{}\t{}",
            task.0,
            to_hex(name),
            to_hex(&criteria.join("\n"))
        ),
        CensorReported {
            task,
            censor,
            verdict,
            note,
            evidence,
            model,
            usage,
        } => {
            let tests = match evidence.tests {
                None => "-",
                Some(true) => "1",
                Some(false) => "0",
            };
            let line = format!(
                "v1\tCENSURED\t{}\t{}\t{}\t{}\t{}\t{}\t{tests}\t{}\t{}",
                task.0,
                censor.0,
                verdict.code(),
                to_hex(note),
                evidence.cited,
                evidence.missing,
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        ChangesDiscarded { task } => format!("v1\tDISCARDED\t{}", task.0),
        SuspicionReviewed {
            task,
            index,
            reviewer,
            upheld,
            note,
            model,
            usage,
        } => {
            let line = format!(
                "v1\tRECHECKED\t{}\t{index}\t{}\t{}\t{}\t{}\t{}",
                task.0,
                reviewer.0,
                u8::from(*upheld),
                to_hex(note),
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        SuspicionAcquitted { task, index } => format!("v1\tACQUITTED\t{}\t{index}", task.0),
        SuspicionRaised {
            task,
            source,
            threat,
            excerpt,
        } => format!(
            "v1\tSUSPECT\t{}\t{}\t{}\t{}",
            task.0,
            threat.code(),
            to_hex(source),
            to_hex(excerpt)
        ),
        ModelUsage { task, usage, trace } => {
            let line = format!("v1\tUSAGE\t{}\t{}\t{}", task.0, usage.input, usage.output);
            match trace {
                Some(trace) => format!("{line}\t{}", to_hex(trace)),
                None => line,
            }
        }
        Clock(at) => format!("v1\tCLOCK\t{at}"),
        TaskReviewed {
            task,
            verdict,
            note,
        } => format!(
            "v1\tREVIEWED\t{}\t{}\t{}",
            task.0,
            verdict.code(),
            to_hex(note)
        ),
    }
}
/// 玉玺占两个字段：工具代码列表、hex 编码的路径列表。
fn split_criteria(joined: &str) -> Vec<String> {
    joined.split('\n').map(str::to_owned).collect()
}

/// 玉玺编码为「工具列表\t范围列表」；MCP 授权以 `mcp:` 前缀并入工具列表（v0.4 及更早没有）。
fn encode_seal(seal: &Seal) -> String {
    let mut tools: Vec<String> = seal
        .capabilities
        .iter()
        .map(|c| c.code().to_owned())
        .collect();
    tools.extend(seal.mcp.iter().map(|m| format!("mcp:{m}")));
    let scope: Vec<_> = seal.scope.iter().map(|p| to_hex(p)).collect();
    format!("{}\t{}", tools.join(","), scope.join(","))
}

fn decode_seal(tools: &str, scope: &str) -> Result<Seal, String> {
    let (mcp, tools): (Vec<&str>, Vec<&str>) =
        split_list(tools).partition(|t| t.starts_with("mcp:"));
    Ok(Seal {
        capabilities: tools
            .into_iter()
            .map(|c| c.parse::<Capability>().map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?,
        scope: split_list(scope).map(from_hex).collect::<Result<_, _>>()?,
        mcp: mcp
            .into_iter()
            .map(|m| m["mcp:".len()..].to_owned())
            .collect(),
    })
}

/// 发言（或殿试答卷）为 `官员ID;官职;内容hex;输入;输出;模型hex（无模型为空）`，以逗号分隔。
fn encode_speeches(speeches: &[Speech]) -> String {
    let parts: Vec<_> = speeches
        .iter()
        .map(|s| {
            format!(
                "{};{};{};{};{};{}",
                s.speaker.0,
                s.office.code(),
                to_hex(&s.text),
                s.usage.input,
                s.usage.output,
                s.model.as_deref().map(to_hex).unwrap_or_default()
            )
        })
        .collect();
    parts.join(",")
}

fn decode_speeches(field: &str) -> Result<Vec<Speech>, String> {
    split_list(field)
        .map(|entry| {
            let [speaker, office, text, input, output, model] =
                entry.split(';').collect::<Vec<_>>()[..]
            else {
                return Err("发言字段不合法".into());
            };
            let number = |s: &str| s.parse::<u64>().map_err(|e| e.to_string());
            Ok(Speech {
                speaker: AgentId(number(speaker)?),
                office: office.parse::<Office>().map_err(|e| e.to_string())?,
                text: from_hex(text)?,
                usage: TokenUsage {
                    input: number(input)?,
                    output: number(output)?,
                },
                model: match model {
                    "" => None,
                    model => Some(from_hex(model)?),
                },
            })
        })
        .collect()
}

/// 廷推的各票为 `投票者;候选人;理由hex;输入;输出;模型hex（无模型为空）`，以逗号分隔。
fn encode_votes(votes: &[Nomination]) -> String {
    let parts: Vec<_> = votes
        .iter()
        .map(|v| {
            format!(
                "{};{};{};{};{};{}",
                v.voter.0,
                v.candidate.0,
                to_hex(&v.note),
                v.usage.input,
                v.usage.output,
                v.model.as_deref().map(to_hex).unwrap_or_default()
            )
        })
        .collect();
    parts.join(",")
}

fn decode_votes(field: &str) -> Result<Vec<Nomination>, String> {
    split_list(field)
        .map(|entry| {
            let [voter, candidate, note, input, output, model] =
                entry.split(';').collect::<Vec<_>>()[..]
            else {
                return Err("廷推字段不合法".into());
            };
            let number = |s: &str| s.parse::<u64>().map_err(|e| e.to_string());
            Ok(Nomination {
                voter: AgentId(number(voter)?),
                candidate: AgentId(number(candidate)?),
                note: from_hex(note)?,
                usage: TokenUsage {
                    input: number(input)?,
                    output: number(output)?,
                },
                model: match model {
                    "" => None,
                    model => Some(from_hex(model)?),
                },
            })
        })
        .collect()
}

/// 逗号分隔的列表；空字段为空列表。
fn split_list(field: &str) -> impl Iterator<Item = &str> {
    field.split(',').filter(|item| !item.is_empty())
}

/// 解码失败的原因。
#[derive(Debug)]
pub(crate) enum DecodeError {
    /// 更新版本的紫垣写入的内容（更高的格式版本或未知的事件类型）：须升级才能读取。
    Newer(String),
    Malformed(String),
}
impl From<String> for DecodeError {
    fn from(reason: String) -> Self {
        Self::Malformed(reason)
    }
}
impl From<&str> for DecodeError {
    fn from(reason: &str) -> Self {
        Self::Malformed(reason.into())
    }
}

/// 事件簿的兼容规则：新增事件类型，或在行尾追加可选字段，格式版本仍为 v1；
/// 旧版读到不认识的事件类型或更高的版本时拒绝读取并提示升级，而不是当作损坏。
/// 只有改变既有字段含义时才升级格式版本，届时由读取方把旧行转换为新事件。
pub(crate) fn decode_event(line: &str) -> Result<CourtEvent, DecodeError> {
    use CourtEvent::*;
    let parts: Vec<&str> = line.split('\t').collect();
    if parts[0] != "v1" {
        let newer = parts[0]
            .strip_prefix('v')
            .and_then(|n| n.parse::<u32>().ok())
            .is_some_and(|n| n > 1);
        return Err(match newer {
            true => DecodeError::Newer(format!("事件格式 {}", parts[0])),
            false => "不支持的事件格式版本".into(),
        });
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
        "PURGED" => OfficialPurged {
            agent: AgentId(id(2)?),
        },
        "APPROVED" => EdictApproved {
            task: TaskId(id(2)?),
        },
        "DRAFTED" => EdictDrafted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            note: from_hex(field(4)?)?,
            usage: TokenUsage {
                input: id(5)?,
                output: id(6)?,
            },
            model: match parts.len() {
                7 => None,
                _ => Some(from_hex(field(7)?)?),
            },
        },
        "RETREAT" => EmperorRetreat(match field(2)? {
            "1" => true,
            "0" => false,
            other => return Err(format!("修道标记无效: {other}").into()),
        }),
        "REFORMED" => InstitutionReformed(
            field(2)?
                .parse::<GovernanceMode>()
                .map_err(|e| e.to_string())?,
        ),
        "EDICT" => EdictIssued(Edict {
            id: TaskId(id(2)?),
            department: field(3)?.parse::<Department>().map_err(|e| e.to_string())?,
            text: from_hex(field(4)?)?,
            // 早期日志没有玉玺字段：当时的诏令实际拥有全部只读工具与整个项目。
            seal: if parts.len() == 5 {
                Seal::default()
            } else {
                decode_seal(field(5)?, field(6)?)?
            },
        }),
        "MODEL" => OfficeModelSet {
            office: field(2)?.parse::<Office>().map_err(|e| e.to_string())?,
            model: match parts.len() {
                3 => None,
                _ => Some(from_hex(field(3)?)?),
            },
        },
        "BUDGET" => BudgetSet {
            scope: match field(2)?.split_once(':') {
                None if field(2)? == "court" => BudgetScope::Court,
                Some(("office", office)) => {
                    BudgetScope::Office(office.parse::<Office>().map_err(|e| e.to_string())?)
                }
                Some(("task", task)) => {
                    BudgetScope::Task(TaskId(task.parse::<u64>().map_err(|e| e.to_string())?))
                }
                _ => return Err(format!("度支范围无效: {}", field(2)?).into()),
            },
            tokens: match parts.len() {
                3 => None,
                _ => Some(id(3)?),
            },
        },
        "CEILING" => OfficeCeilingSet {
            office: field(2)?.parse::<Office>().map_err(|e| e.to_string())?,
            ceiling: match parts.len() {
                3 => None,
                _ => Some(decode_seal(field(3)?, field(4)?)?),
            },
        },
        "COMPLETED" => TaskCompleted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            summary: from_hex(field(4)?)?,
        },
        // v0.2 及更早的日志没有模型字段。
        "ASSIGNED" => TaskAssigned {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            model: match parts.len() {
                4 => None,
                _ => Some(from_hex(field(4)?)?),
            },
        },
        "TOOL" => ToolInvoked {
            task: TaskId(id(2)?),
            tool: from_hex(field(3)?)?,
            args_json: from_hex(field(4)?)?,
            ok: match field(5)? {
                "1" => true,
                "0" => false,
                other => return Err(format!("成败标记无效: {other}").into()),
            },
        },
        "FAILED" => TaskFailed {
            task: TaskId(id(2)?),
            reason: from_hex(field(3)?)?,
        },
        "CANCELLED" => TaskCancelled {
            task: TaskId(id(2)?),
        },
        "CHANGES" => ChangesProposed {
            task: TaskId(id(2)?),
            changes: split_list(field(3)?)
                .map(|entry| -> Result<FileChange, String> {
                    let [path, before, after] = entry.split(':').collect::<Vec<_>>()[..] else {
                        return Err("改动字段不合法".into());
                    };
                    Ok(FileChange {
                        path: from_hex(path)?,
                        before: (before != "-").then(|| from_hex(before)).transpose()?,
                        after: from_hex(after)?,
                    })
                })
                .collect::<Result<_, _>>()?,
        },
        "APPLIED" => ChangesApplied {
            task: TaskId(id(2)?),
            skipped: match parts.len() {
                3 => Vec::new(),
                _ => split_list(field(3)?)
                    .map(|n| n.parse().map_err(|_| format!("段号无效: {n}")))
                    .collect::<Result<_, _>>()?,
            },
        },
        "SPLIT" => EdictSplit {
            task: TaskId(id(2)?),
            subtasks: split_list(field(3)?)
                .map(|entry| -> Result<Subtask, String> {
                    let [sub, worker, text] = entry.split(':').collect::<Vec<_>>()[..] else {
                        return Err("子诏令字段不合法".into());
                    };
                    let number = |s: &str| s.parse::<u64>().map_err(|e| e.to_string());
                    Ok(Subtask {
                        id: TaskId(number(sub)?),
                        worker: AgentId(number(worker)?),
                        text: from_hex(text)?,
                    })
                })
                .collect::<Result<_, _>>()?,
            note: from_hex(field(4)?)?,
            usage: TokenUsage {
                input: id(5)?,
                output: id(6)?,
            },
            model: match parts.len() {
                7 => None,
                _ => Some(from_hex(field(7)?)?),
            },
        },
        "DEBATED" => CourtDebated {
            task: TaskId(id(2)?),
            speeches: decode_speeches(field(3)?)?,
        },
        "EXAM" => ExamTaken {
            agent: AgentId(id(2)?),
            exam: from_hex(field(3)?)?,
            score: field(4)?
                .parse::<u8>()
                .ok()
                .filter(|s| *s <= 100)
                .ok_or("分数须为 0–100")?,
            usage: TokenUsage {
                input: id(5)?,
                output: id(6)?,
            },
            answer: from_hex(field(7)?)?,
            model: match parts.len() {
                8 => None,
                _ => Some(from_hex(field(8)?)?),
            },
        },
        "PALACE" => PalaceExamHeld {
            question: from_hex(field(2)?)?,
            answers: decode_speeches(field(3)?)?,
        },
        "ZHUANGYUAN" => ZhuangyuanNamed {
            agent: AgentId(id(2)?),
        },
        "COURIER" => CourierDelivered {
            task: TaskId(id(2)?),
            station: from_hex(field(3)?)?,
        },
        "REPLIED" => CourierReplied {
            task: TaskId(id(2)?),
            pull_request: match field(3)? {
                "pr" => true,
                "comment" => false,
                other => return Err(format!("回报类型无效: {other}").into()),
            },
            url: from_hex(field(4)?)?,
        },
        "URGENT" => EdictExpedited {
            task: TaskId(id(2)?),
        },
        "TEMPLATE" => TemplateSaved {
            name: from_hex(field(2)?)?,
            template: Template {
                department: field(3)?.parse::<Department>().map_err(|e| e.to_string())?,
                text: from_hex(field(4)?)?,
                seal: decode_seal(field(5)?, field(6)?)?,
                criteria: split_criteria(&from_hex(field(7)?)?),
            },
        },
        "UNTEMPLATE" => TemplateRemoved {
            name: from_hex(field(2)?)?,
        },
        "NOMINATED" => CourtNominated {
            office: field(2)?.parse::<Office>().map_err(|e| e.to_string())?,
            votes: decode_votes(field(3)?)?,
        },
        "CANED" => OfficialCaned {
            agent: AgentId(id(2)?),
        },
        "PARDONED" => OfficialPardoned {
            agent: AgentId(id(2)?),
        },
        "TEMPLATED" => TemplateApplied {
            task: TaskId(id(2)?),
            name: from_hex(field(3)?)?,
            criteria: split_criteria(&from_hex(field(4)?)?),
        },
        "CENSURED" => CensorReported {
            task: TaskId(id(2)?),
            censor: AgentId(id(3)?),
            verdict: field(4)?
                .parse::<CensorVerdict>()
                .map_err(|e| e.to_string())?,
            note: from_hex(field(5)?)?,
            evidence: Evidence {
                cited: u32::try_from(id(6)?).map_err(|e| e.to_string())?,
                missing: u32::try_from(id(7)?).map_err(|e| e.to_string())?,
                tests: match field(8)? {
                    "-" => None,
                    "1" => Some(true),
                    "0" => Some(false),
                    other => return Err(format!("测试标记无效: {other}").into()),
                },
            },
            usage: TokenUsage {
                input: id(9)?,
                output: id(10)?,
            },
            model: match parts.len() {
                11 => None,
                _ => Some(from_hex(field(11)?)?),
            },
        },
        "DISCARDED" => ChangesDiscarded {
            task: TaskId(id(2)?),
        },
        "RECHECKED" => SuspicionReviewed {
            task: TaskId(id(2)?),
            index: u32::try_from(id(3)?).map_err(|e| e.to_string())?,
            reviewer: AgentId(id(4)?),
            upheld: match field(5)? {
                "1" => true,
                "0" => false,
                other => return Err(format!("复勘结论无效: {other}").into()),
            },
            note: from_hex(field(6)?)?,
            usage: TokenUsage {
                input: id(7)?,
                output: id(8)?,
            },
            model: match parts.len() {
                9 => None,
                _ => Some(from_hex(field(9)?)?),
            },
        },
        "ACQUITTED" => SuspicionAcquitted {
            task: TaskId(id(2)?),
            index: u32::try_from(id(3)?).map_err(|e| e.to_string())?,
        },
        "SUSPECT" => SuspicionRaised {
            task: TaskId(id(2)?),
            threat: field(3)?.parse::<Threat>().map_err(|e| e.to_string())?,
            source: from_hex(field(4)?)?,
            excerpt: from_hex(field(5)?)?,
        },
        // v0.4 及更早的日志没有 trace 字段。
        "USAGE" => ModelUsage {
            task: TaskId(id(2)?),
            usage: TokenUsage {
                input: id(3)?,
                output: id(4)?,
            },
            trace: match parts.len() {
                5 => None,
                _ => Some(from_hex(field(5)?)?),
            },
        },
        "CLOCK" => Clock(id(2)?),
        "REVIEWED" => TaskReviewed {
            task: TaskId(id(2)?),
            verdict: field(3)?.parse::<Verdict>().map_err(|e| e.to_string())?,
            note: from_hex(field(4)?)?,
        },
        other => return Err(DecodeError::Newer(format!("事件类型 {other}"))),
    };
    let expected_fields = match &event {
        Founded(_)
        | InstitutionReformed(_)
        | EmperorRetreat(_)
        | OfficialDismissed { .. }
        | OfficialPurged { .. }
        | EdictApproved { .. }
        | ChangesDiscarded { .. }
        | TaskCancelled { .. } => 3,
        ChangesApplied { skipped, .. } if skipped.is_empty() => 3,
        ChangesApplied { .. } => 4,
        ChangesProposed { .. } | CourtDebated { .. } | PalaceExamHeld { .. } => 4,
        ZhuangyuanNamed { .. } | EdictExpedited { .. } => 3,
        CourierDelivered { .. } => 4,
        CourierReplied { .. } => 5,
        TemplateRemoved { .. } | OfficialCaned { .. } | OfficialPardoned { .. } => 3,
        CourtNominated { .. } => 4,
        TemplateApplied { .. } => 5,
        TemplateSaved { .. } => 8,
        ExamTaken { model: None, .. } => 8,
        ExamTaken { .. } => 9,
        CensorReported { model: None, .. } => 11,
        EdictSplit { model: None, .. } => 7,
        EdictSplit { .. } => 8,
        CensorReported { .. } => 12,
        SuspicionReviewed { model: None, .. } => 9,
        SuspicionReviewed { .. } => 10,
        SuspicionAcquitted { .. } => 4,
        OfficeModelSet { model: None, .. }
        | OfficeCeilingSet { ceiling: None, .. }
        | BudgetSet { tokens: None, .. } => 3,
        BudgetSet { .. } => 4,
        OfficialAppointed { .. }
        | TaskAssigned { model: None, .. }
        | OfficeModelSet { .. }
        | TaskFailed { .. } => 4,
        TaskAssigned { .. } => 5,
        EdictDrafted { model: None, .. } => 7,
        EdictDrafted { .. } => 8,
        OfficeCeilingSet { .. } => 5,
        EdictIssued(_) if parts.len() == 5 => 5,
        ModelUsage { trace: Some(_), .. } => 6,
        AgentRegistered(_) | TaskCompleted { .. } | TaskReviewed { .. } | ModelUsage { .. } => 5,
        Clock(_) => 3,
        EdictIssued(_) => 7,
        ToolInvoked { .. } | SuspicionRaised { .. } => 6,
    };
    if parts.len() != expected_fields {
        return Err("字段数量不正确".into());
    }
    Ok(event)
}

/// 能读这一行的最早发布版本（`0.x.0` 的 `x`）。逐一用 v0.1–v0.5 发布时的解码器试读得出，
/// 样本见 `compat.tsv`。此后每新增事件类型、字段或代码，都要在这里与样本中记上新版本；
/// 这里不用通配，新增事件类型时编译器会提醒。
pub(crate) fn readable_since(line: &str) -> Result<u32, DecodeError> {
    use CourtEvent::*;
    let event = decode_event(line)?;
    // 外藩授权（`mcp:`）自 v0.5 可读，写权限与跑测试自 v0.3。
    let seal = |seal: &Seal, base: u32| {
        let write = seal
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::WriteFile | Capability::RunTests));
        if !seal.mcp.is_empty() {
            5
        } else if write {
            base.max(3)
        } else {
            base
        }
    };
    Ok(match &event {
        Founded(_)
        | AgentRegistered(_)
        | OfficialDismissed { .. }
        | InstitutionReformed(_)
        | TaskCompleted { .. } => 1,
        OfficialAppointed { office, .. } => match office {
            Office::ViceChancellor | Office::SecondGrandSecretary | Office::ViceMinister(_) => 2,
            Office::Censor => 3,
            _ => 1,
        },
        // 不带玉玺字段的旧式诏令 v0.1 即可读；带玉玺的自 v0.2。
        EdictIssued(e) => seal(&e.seal, if line.split('\t').count() == 5 { 1 } else { 2 }),
        ModelUsage { trace: Some(_), .. } => 5,
        ChangesApplied { skipped, .. } if !skipped.is_empty() => 4,
        OfficeCeilingSet {
            ceiling: Some(s), ..
        } => seal(s, 3),
        TemplateSaved { template, .. } => seal(&template.seal, 4),
        EdictApproved { .. }
        | TaskAssigned { model: None, .. }
        | ToolInvoked { .. }
        | TaskFailed { .. }
        | TaskCancelled { .. }
        | OfficialPurged { .. }
        | TaskReviewed { .. }
        | ModelUsage { .. } => 2,
        TaskAssigned { .. }
        | OfficeModelSet { .. }
        | OfficeCeilingSet { .. }
        | EmperorRetreat(_)
        | EdictDrafted { .. }
        | EdictSplit { .. }
        | CourtDebated { .. }
        | ChangesProposed { .. }
        | ChangesApplied { .. }
        | ChangesDiscarded { .. }
        | CensorReported { .. } => 3,
        BudgetSet { .. }
        | SuspicionRaised { .. }
        | ExamTaken { .. }
        | PalaceExamHeld { .. }
        | ZhuangyuanNamed { .. }
        | CourierDelivered { .. }
        | EdictExpedited { .. }
        | CourtNominated { .. }
        | OfficialCaned { .. }
        | OfficialPardoned { .. }
        | TemplateRemoved { .. }
        | TemplateApplied { .. } => 4,
        Clock(_) | SuspicionReviewed { .. } | SuspicionAcquitted { .. } | CourierReplied { .. } => {
            5
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readable_since_matches_what_released_decoders_could_read() {
        let mut seen = std::collections::BTreeSet::new();
        for row in include_str!("compat.tsv").lines() {
            if row.starts_with('#') {
                continue;
            }
            let (version, line) = row.split_once('\t').unwrap();
            let minor: u32 = version.strip_prefix("0.").unwrap().parse().unwrap();
            // 旧版写下的每种样本，本版都读得回来。
            assert_eq!(readable_since(line).unwrap(), minor, "{line}");
            seen.insert(line.split('\t').nth(1).unwrap().to_owned());
        }
        assert_eq!(seen.len(), 42, "每种事件类型至少一条样本");
    }
    #[test]
    fn event_round_trip_chinese_and_tabs() {
        let task = TaskId(42);
        let events = [
            CourtEvent::EdictIssued(Edict {
                id: task,
                department: Department::Works,
                text: "检查\tRust\n测试".into(),
                seal: Seal {
                    capabilities: [Capability::SearchCode].into(),
                    scope: vec!["crates/紫垣,a".into(), "README.md".into()],
                    mcp: ["gh/search".to_owned(), "docs".to_owned()].into(),
                },
            }),
            CourtEvent::EdictIssued(Edict {
                id: TaskId(43),
                department: Department::Works,
                text: "无工具".into(),
                seal: Seal {
                    capabilities: Default::default(),
                    scope: vec![".".into()],
                    mcp: Default::default(),
                },
            }),
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(7),
                model: None,
            },
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(7),
                model: Some("deepseek-flash".into()),
            },
            CourtEvent::OfficeModelSet {
                office: Office::Clerk(Department::Works),
                model: Some("deepseek-v4-pro".into()),
            },
            CourtEvent::OfficeModelSet {
                office: Office::Chancellor,
                model: None,
            },
            CourtEvent::OfficeCeilingSet {
                office: Office::Clerk(Department::Works),
                ceiling: Some(Seal {
                    capabilities: [Capability::ReadFile].into(),
                    scope: vec!["crates/紫垣".into()],
                    mcp: Default::default(),
                }),
            },
            CourtEvent::OfficeCeilingSet {
                office: Office::Minister(Department::Works),
                ceiling: None,
            },
            CourtEvent::SuspicionRaised {
                task,
                source: "tool:read_file".into(),
                threat: Threat::Hijack,
                excerpt: "忽略以上\t,规定".into(),
            },
            CourtEvent::SuspicionReviewed {
                task,
                index: 0,
                reviewer: AgentId(5),
                upheld: false,
                note: "误\t报".into(),
                model: Some("flash".into()),
                usage: TokenUsage {
                    input: 9,
                    output: 2,
                },
            },
            CourtEvent::SuspicionReviewed {
                task,
                index: 1,
                reviewer: AgentId(5),
                upheld: true,
                note: String::new(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::SuspicionAcquitted { task, index: 0 },
            CourtEvent::ExamTaken {
                agent: AgentId(3),
                exam: "事件簿\t,一".into(),
                score: 75,
                answer: "答：\t,；".into(),
                model: Some("flash".into()),
                usage: TokenUsage {
                    input: 8,
                    output: 2,
                },
            },
            CourtEvent::ExamTaken {
                agent: AgentId(3),
                exam: "x".into(),
                score: 0,
                answer: String::new(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::PalaceExamHeld {
                question: "何为治国；之本,".into(),
                answers: vec![Speech {
                    speaker: AgentId(2),
                    office: Office::Censor,
                    text: "民为邦本".into(),
                    model: None,
                    usage: TokenUsage::default(),
                }],
            },
            CourtEvent::ZhuangyuanNamed { agent: AgentId(2) },
            CourtEvent::CourierDelivered {
                task,
                station: "CI\t驿站".into(),
            },
            CourtEvent::CourierReplied {
                task,
                pull_request: true,
                url: "https://github.com/o/r/pull/9".into(),
            },
            CourtEvent::CourierReplied {
                task,
                pull_request: false,
                url: "https://x/\t,".into(),
            },
            CourtEvent::EdictExpedited { task },
            CourtEvent::TemplateSaved {
                name: "补\t测试".into(),
                template: Template {
                    department: Department::Works,
                    text: "补测试\n两行".into(),
                    seal: Seal {
                        capabilities: [Capability::WriteFile, Capability::RunTests].into(),
                        scope: vec!["crates/a b".into(), "README.md".into()],
                        mcp: Default::default(),
                    },
                    criteria: vec!["测试通过".into(), "不改行为".into()],
                },
            },
            CourtEvent::TemplateRemoved {
                name: "补\t测试".into(),
            },
            CourtEvent::CourtNominated {
                office: Office::Minister(Department::Works),
                votes: vec![
                    Nomination {
                        voter: AgentId(1),
                        candidate: AgentId(4),
                        note: "精细,平实;可任".into(),
                        model: Some("deepseek-flash".into()),
                        usage: TokenUsage {
                            input: 30,
                            output: 5,
                        },
                    },
                    Nomination {
                        voter: AgentId(3),
                        candidate: AgentId(4),
                        note: String::new(),
                        model: None,
                        usage: TokenUsage::default(),
                    },
                ],
            },
            CourtEvent::OfficialCaned { agent: AgentId(4) },
            CourtEvent::OfficialPardoned { agent: AgentId(4) },
            CourtEvent::TemplateApplied {
                task,
                name: "审查unsafe".into(),
                criteria: vec!["列出行号".into()],
            },
            CourtEvent::BudgetSet {
                scope: BudgetScope::Court,
                tokens: Some(1_000_000),
            },
            CourtEvent::BudgetSet {
                scope: BudgetScope::Office(Office::Minister(Department::Works)),
                tokens: Some(20_000),
            },
            CourtEvent::BudgetSet {
                scope: BudgetScope::Task(task),
                tokens: None,
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
            CourtEvent::EmperorRetreat(true),
            CourtEvent::EmperorRetreat(false),
            CourtEvent::ChangesProposed {
                task,
                changes: vec![
                    FileChange {
                        path: "src/新:a.rs".into(),
                        before: None,
                        after: "fn a() {}\n".into(),
                    },
                    FileChange {
                        path: "README.md".into(),
                        before: Some(String::new()),
                        after: "\t,一\n".into(),
                    },
                ],
            },
            CourtEvent::ChangesApplied {
                task,
                skipped: Vec::new(),
            },
            CourtEvent::ChangesApplied {
                task,
                skipped: vec![2, 5],
            },
            CourtEvent::EdictSplit {
                task,
                subtasks: vec![
                    Subtask {
                        id: TaskId(8),
                        worker: AgentId(4),
                        text: "查 storage：\t,一".into(),
                    },
                    Subtask {
                        id: TaskId(9),
                        worker: AgentId(7),
                        text: "查 runtime".into(),
                    },
                ],
                note: "分而治之".into(),
                model: Some("pro".into()),
                usage: TokenUsage {
                    input: 5,
                    output: 1,
                },
            },
            CourtEvent::EdictSplit {
                task,
                subtasks: vec![],
                note: String::new(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::CourtDebated {
                task,
                speeches: vec![
                    Speech {
                        speaker: AgentId(2),
                        office: Office::Minister(Department::Works),
                        text: "臣以为；不可：\t,急".into(),
                        model: Some("pro".into()),
                        usage: TokenUsage {
                            input: 9,
                            output: 3,
                        },
                    },
                    Speech {
                        speaker: AgentId(5),
                        office: Office::Censor,
                        text: String::new(),
                        model: None,
                        usage: TokenUsage::default(),
                    },
                ],
            },
            CourtEvent::CensorReported {
                task,
                censor: AgentId(7),
                verdict: CensorVerdict::Impeached,
                note: "查无\t实据".into(),
                evidence: Evidence {
                    cited: 3,
                    missing: 1,
                    tests: Some(false),
                },
                model: Some("pro".into()),
                usage: TokenUsage {
                    input: 9,
                    output: 2,
                },
            },
            CourtEvent::CensorReported {
                task,
                censor: AgentId(7),
                verdict: CensorVerdict::Endorsed,
                note: String::new(),
                evidence: Evidence::default(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::ChangesDiscarded { task },
            CourtEvent::EdictApproved { task },
            CourtEvent::EdictDrafted {
                task,
                worker: AgentId(4),
                note: "拟派：#4\t熟悉此事".into(),
                model: Some("deepseek-v4-pro".into()),
                usage: TokenUsage {
                    input: 300,
                    output: 20,
                },
            },
            CourtEvent::EdictDrafted {
                task,
                worker: AgentId(7),
                note: "御笔改票".into(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::OfficialPurged { agent: AgentId(3) },
            CourtEvent::ModelUsage {
                task,
                usage: TokenUsage {
                    input: 12_345,
                    output: 678,
                },
                trace: None,
            },
            CourtEvent::ModelUsage {
                task,
                usage: TokenUsage::default(),
                trace: Some("zy-0123456789ab".into()),
            },
            CourtEvent::Clock(1_792_909_102),
            CourtEvent::TaskReviewed {
                task,
                verdict: Verdict::Rejected,
                note: "结论有误\t需重查".into(),
            },
        ];
        for original in events {
            assert_eq!(decode_event(&encode_event(&original)).unwrap(), original);
        }
    }
    #[test]
    fn edict_without_seal_fields_gets_default_seal() {
        let CourtEvent::EdictIssued(edict) = decode_event("v1\tEDICT\t1\tworks\te69fa5").unwrap()
        else {
            panic!("应解码为诏令");
        };
        assert_eq!((edict.text.as_str(), edict.seal), ("查", Seal::default()));
        assert!(decode_event("v1\tEDICT\t1\tworks\te69fa5\tread_file").is_err());
        assert!(decode_event("v1\tEDICT\t1\tworks\te69fa5\trun_shell\t2e").is_err());
    }
    #[test]
    fn malformed_event_is_rejected() {
        let malformed = |line| matches!(decode_event(line), Err(DecodeError::Malformed(_)));
        let newer = |line| matches!(decode_event(line), Err(DecodeError::Newer(_)));
        assert!(malformed("v1\tEDICT\t42\tworks\tbroken"));
        assert!(malformed("v1\tEXAM\t3\t78\t101\t0\t0\t"), "分数超过 100");
        assert!(malformed("v1\tEDICT\t42\tworks\te69g"), "非法 hex 字符");
        assert!(malformed("v1\tEDICT\t42\tworks\te69"), "hex 长度为奇数");
        assert!(malformed("v0\tFOUNDED\tcabinet") && malformed("x\tFOUNDED"));
        // 更新版本写入的：更高的格式版本、不认识的事件类型，提示升级而非损坏。
        assert!(newer("v2\tFOUNDED\tcabinet"));
        assert!(newer("v1\tCOURT_BANQUET\t1"));
        assert_eq!(
            decode_event("v1\tRETREAT\t1").unwrap(),
            CourtEvent::EmperorRetreat(true)
        );
        assert!(
            matches!(
                decode_event("v1\tCOMPLETED\t1\t4\t4142").unwrap(),
                CourtEvent::TaskCompleted { summary, .. } if summary == "AB"
            ),
            "大写 hex 也可读"
        );
    }
}
