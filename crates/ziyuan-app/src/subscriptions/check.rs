use super::{model::*, store::Subscriptions};
use std::collections::{HashMap, HashSet};

impl Subscriptions {
    /// No state lock is held while calling the source. At most one batch of 50 per invocation.
    /// Manual checks bypass scheduling, but not the 60 second per-work cooldown or live leases.
    /// Scheduled errors back off by 1800, 3600, then 7200 seconds (maximum).
    pub fn check(
        &self,
        source: &impl WorkSource,
        now: u64,
        mode: CheckMode,
    ) -> Result<CheckSummary, SubscriptionError> {
        let (_, (claims, mut summary)) = self.file.update::<_, SubscriptionError>(|current| {
            let mut state = current.map(|s| s.value).unwrap_or_default();
            let mut summary = CheckSummary::default();
            let mut claims = Vec::new();
            if mode == CheckMode::Scheduled && !state.settings.enabled {
                summary.status = CheckStatus::Disabled;
            } else if state.items.is_empty() {
                summary.status = CheckStatus::Empty;
            } else {
                for item in &mut state.items {
                    let leased = item.lease.as_ref().is_some_and(|l| l.expires > now);
                    let cooldown = item
                        .last_attempt
                        .is_some_and(|at| now < at.saturating_add(60));
                    let not_due = mode == CheckMode::Scheduled && now < item.next_check;
                    if leased || cooldown || not_due || claims.len() == 50 {
                        summary.skipped += 1;
                        continue;
                    }
                    item.lease = Some(Lease {
                        token: random_id()?,
                        expires: now.saturating_add(90),
                    });
                    item.last_attempt = Some(now);
                    claims.push(item.clone());
                }
                if claims.is_empty() {
                    summary.status = CheckStatus::NotDue;
                }
            }
            Ok((state, (claims, summary)))
        })?;
        if claims.is_empty() {
            return Ok(summary);
        }
        let codes: Vec<_> = claims.iter().map(|i| i.code.clone()).collect();
        let response = source.fetch(&codes).and_then(|works| {
            let requested: HashSet<_> = codes.iter().collect();
            let mut found = HashSet::new();
            if works
                .iter()
                .any(|w| !requested.contains(&w.code) || !found.insert(w.code.clone()))
            {
                Err(SourceError::Malformed)
            } else {
                Ok(works
                    .into_iter()
                    .map(|w| (w.code.clone(), w))
                    .collect::<HashMap<_, _>>())
            }
        });
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                for claim in claims {
                    let Some(item) = state.items.iter_mut().find(|i| {
                        i.id == claim.id
                            && i.code == claim.code
                            && i.lease == claim.lease
                            && i.baseline_revision == claim.baseline_revision
                    }) else {
                        summary.discarded += 1;
                        continue;
                    };
                    summary.checked += 1;
                    let observed = match &response {
                        Err(e) => Err(e.clone()),
                        Ok(works) => works.get(&item.code).cloned().ok_or(SourceError::NotFound),
                    }
                    .and_then(|w| {
                        if w.valid() {
                            Ok(w)
                        } else {
                            Err(SourceError::Malformed)
                        }
                    })
                    .and_then(|w| {
                        if item.baseline.as_ref().is_some_and(|old| {
                            w.chapter_count < old.chapter_count
                                || w.last_published < old.last_published
                                || w.work_updated < old.work_updated
                        }) {
                            Err(SourceError::Suspicious)
                        } else {
                            Ok(w)
                        }
                    });
                    item.lease = None;
                    match observed {
                        Err(error) => {
                            summary.failed += 1;
                            item.last_error = Some(error);
                            item.consecutive_failures = item.consecutive_failures.saturating_add(1);
                            let backoff =
                                1800_u64 << item.consecutive_failures.saturating_sub(1).min(2);
                            item.next_check = now.saturating_add(backoff);
                        }
                        Ok(work) => {
                            let kind = item.baseline.as_ref().and_then(|old| {
                                if work.chapter_count > old.chapter_count {
                                    Some(NoticeKind::NewChapters {
                                        observed_increase: work.chapter_count - old.chapter_count,
                                    })
                                } else if old != &work {
                                    Some(NoticeKind::InformationChanged)
                                } else {
                                    None
                                }
                            });
                            if let Some(kind) = kind {
                                state.notices.push(Notice {
                                    id: random_id()?,
                                    subscription_id: item.id.clone(),
                                    code: item.code.clone(),
                                    observed_at: now,
                                    kind,
                                    observation: work.clone(),
                                    read: false,
                                });
                                summary.notices += 1;
                            }
                            item.baseline = Some(work);
                            item.baseline_revision = item.baseline_revision.saturating_add(1);
                            item.last_error = None;
                            item.consecutive_failures = 0;
                            item.next_check = now.saturating_add(state.settings.interval_seconds);
                        }
                    }
                }
                Ok((state, summary))
            })
            .map(|(_, result)| result)
    }
}
