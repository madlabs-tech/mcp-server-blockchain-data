//! `compliance` tools. See the ownership table in `ops/mod.rs`.

use super::stablecoin::{controlled, parse_account, restrictions_for, TokenError};
use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ems_domain::{AccountId, Attempt, AttemptOutcome, DomainError, Provenance, SourceKind};
use ems_ports::{Capability, ProviderError, SanctionsScreener, ScreenResult};
use ems_protocols::issuer::Restrictions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub fn register(c: &mut Catalog) {
    c.register(ScreenAddress);
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenIn {
    /// Chain alias or CAIP-2 id.
    pub chain: String,
    /// Address to screen (EVM address; Solana owner wallet).
    pub address: String,
    /// Also check issuer freezes/blacklists on every registered stablecoin (default true).
    #[serde(default)]
    pub include_issuer_freeze: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// At least one source flagged the address (sanctioned, or frozen by a stablecoin issuer).
    Blocked,
    /// At least one sanctions source answered and nothing was flagged.
    Clear,
    /// No sanctions source could answer: do not treat as clear.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindOut {
    Sanctions,
    IssuerFreeze,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Hit,
    Clear,
    Error,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SourceResult {
    /// Vendor id (`chainalysis_oracle`, `trm`) or `issuer:<SYMBOL>`.
    pub source: String,
    pub kind: SourceKindOut,
    pub status: SourceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ScreenOut {
    /// CAIP-10 account screened.
    pub subject: AccountId,
    pub verdict: Verdict,
    pub sanctioned: bool,
    pub issuer_restricted: bool,
    /// Every source consulted, including the ones that failed or were skipped.
    pub sources: Vec<SourceResult>,
    pub as_of: DateTime<Utc>,
}

/// One sanctions vendor's outcome.
pub type SanctionsOutcome = (String, Result<ScreenResult, ProviderError>);

/// Combine sanctions answers and issuer restrictions into one verdict. Pure.
pub fn verdict(
    subject: AccountId,
    sanctions: Vec<SanctionsOutcome>,
    skipped: Vec<Attempt>,
    issuer: &[Restrictions],
    issuer_errors: &[TokenError],
) -> ScreenOut {
    let mut sources = Vec::new();
    let mut answered = 0;
    let mut sanctioned = false;
    for (vendor, r) in sanctions {
        let (status, detail) = match r {
            Ok(s) => {
                answered += 1;
                sanctioned |= s.sanctioned;
                let st = if s.sanctioned {
                    SourceStatus::Hit
                } else {
                    SourceStatus::Clear
                };
                (st, s.detail)
            }
            Err(e) => (SourceStatus::Error, Some(e.to_string())),
        };
        sources.push(SourceResult {
            source: vendor,
            kind: SourceKindOut::Sanctions,
            status,
            detail,
        });
    }
    sources.extend(
        skipped
            .into_iter()
            .filter(|a| a.outcome == AttemptOutcome::Skipped)
            .map(|a| SourceResult {
                source: a.vendor,
                kind: SourceKindOut::Sanctions,
                status: SourceStatus::Skipped,
                detail: a.reason,
            }),
    );
    let mut issuer_restricted = false;
    for r in issuer {
        issuer_restricted |= r.restricted;
        let flagged: Vec<String> = r
            .checks
            .iter()
            .filter(|c| c.restricted)
            .map(|c| c.detail.clone().unwrap_or_else(|| c.method.clone()))
            .collect();
        sources.push(SourceResult {
            source: format!("issuer:{}", r.symbol),
            kind: SourceKindOut::IssuerFreeze,
            status: if r.restricted {
                SourceStatus::Hit
            } else {
                SourceStatus::Clear
            },
            detail: Some(if flagged.is_empty() {
                format!("block {}", r.block.number)
            } else {
                format!("{} (block {})", flagged.join("; "), r.block.number)
            }),
        });
    }
    sources.extend(issuer_errors.iter().map(|e| SourceResult {
        source: format!("issuer:{}", e.symbol),
        kind: SourceKindOut::IssuerFreeze,
        status: SourceStatus::Error,
        detail: Some(e.error.clone()),
    }));
    let verdict = if sanctioned || issuer_restricted {
        Verdict::Blocked
    } else if answered > 0 {
        Verdict::Clear
    } else {
        Verdict::Unknown
    };
    ScreenOut {
        subject,
        verdict,
        sanctioned,
        issuer_restricted,
        sources,
        as_of: Utc::now(),
    }
}

pub struct ScreenAddress;

#[async_trait]
impl Operation for ScreenAddress {
    type Input = ScreenIn;
    type Output = ScreenOut;
    const NAME: &'static str = "compliance_screen_address";
    const DOMAIN: Domain = Domain::Compliance;
    const DESCRIPTION: &'static str = "Screen an address before paying it or accepting its funds: asks every configured sanctions source (Chainalysis on-chain oracle on EVM chains, TRM sanctions API) and checks stablecoin issuer freezes/blacklists on the chain, then returns one verdict (blocked | clear | unknown) with each source's answer listed. \
Use it in deposit and withdrawal flows of payment and neobank agents. \
Caveats: sanctions lists only (not KYT risk scoring); the Chainalysis oracle may lag official lists and is not on Solana or Robinhood Chain; TRM's keyless tier allows 100 requests/day; verdict=unknown means no sanctions source answered, so do not treat it as clear.";
    const PROFILES: &'static [Profile] = &[Profile::Payments, Profile::Neobank];

    async fn execute(
        &self,
        ctx: &Ctx,
        input: ScreenIn,
    ) -> Result<OpOutput<ScreenOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let address = parse_account(chain, &input.address)?;
        let subject = AccountId::new(chain.id.clone(), address)?;

        let req = ctx.route(Capability::Sanctions).chain(chain.id.clone());
        let (sanctions, skipped, mut meta) = match ctx
            .router()
            .fan_out::<dyn SanctionsScreener, _, _, _>(req, |p| {
                let s = subject.clone();
                async move { p.screen(&s).await }
            })
            .await
        {
            Ok(r) => (r.value, r.provenance.providers_tried.clone(), r.provenance),
            Err(e) => {
                // Nobody answered: report every failed/skipped vendor as a source.
                let failed = e
                    .attempts
                    .iter()
                    .filter(|a| a.outcome == AttemptOutcome::Failed)
                    .map(|a| {
                        let msg = a.reason.clone().unwrap_or_else(|| "failed".into());
                        (a.vendor.clone(), Err(ProviderError::Transient(msg)))
                    })
                    .collect();
                let mut p = Provenance::new(SourceKind::Aggregate);
                p.providers_tried = e.attempts.clone();
                (failed, e.attempts, p)
            }
        };

        let (issuer, issuer_errors) = if input.include_issuer_freeze.unwrap_or(true) {
            let entries = controlled(chain)?;
            restrictions_for(ctx, chain, &address, &entries).await?
        } else {
            (Vec::new(), Vec::new())
        };
        meta.chain = Some(chain.id.clone());
        Ok(OpOutput::new(
            verdict(subject, sanctions, skipped, &issuer, &issuer_errors),
            meta,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ems_domain::BlockRef;
    use ems_protocols::issuer::{CheckKind, RestrictionCheck};

    fn subject() -> AccountId {
        "eip155:1:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap()
    }

    fn screen(vendor: &str, hit: bool) -> SanctionsOutcome {
        (
            vendor.into(),
            Ok(ScreenResult {
                sanctioned: hit,
                source: vendor.into(),
                detail: None,
                as_of: Utc::now(),
            }),
        )
    }

    fn restriction(restricted: bool) -> Restrictions {
        Restrictions {
            asset: "eip155:1/erc20:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
                .parse()
                .unwrap(),
            symbol: "USDC".into(),
            address: subject().address,
            restricted,
            block: BlockRef {
                number: 7,
                hash: None,
                timestamp: None,
            },
            checks: vec![RestrictionCheck {
                kind: CheckKind::AddressFrozen,
                method: "isBlacklisted(address)".into(),
                restricted,
                detail: restricted.then(|| "blacklisted by Circle".into()),
            }],
        }
    }

    #[test]
    fn any_sanctions_hit_blocks_and_every_source_is_listed() {
        let out = verdict(
            subject(),
            vec![screen("chainalysis_oracle", false), screen("trm", true)],
            vec![],
            &[restriction(false)],
            &[],
        );
        assert_eq!(out.verdict, Verdict::Blocked);
        assert!(out.sanctioned && !out.issuer_restricted);
        assert_eq!(out.sources.len(), 3);
        assert_eq!(out.sources[1].status, SourceStatus::Hit);
        assert_eq!(out.sources[2].source, "issuer:USDC");
    }

    #[test]
    fn issuer_freeze_alone_blocks() {
        let out = verdict(
            subject(),
            vec![screen("trm", false)],
            vec![],
            &[restriction(true)],
            &[],
        );
        assert_eq!(out.verdict, Verdict::Blocked);
        assert!(out.issuer_restricted);
        assert!(out.sources[1]
            .detail
            .as_ref()
            .unwrap()
            .contains("blacklisted by Circle"));
    }

    #[test]
    fn no_sanctions_answer_is_unknown_not_clear() {
        let skipped = vec![Attempt {
            vendor: "trm".into(),
            outcome: AttemptOutcome::Skipped,
            reason: Some("quota_reserve".into()),
            error_code: None,
            latency_ms: None,
        }];
        let out = verdict(
            subject(),
            vec![(
                "chainalysis_oracle".into(),
                Err(ProviderError::Transient("timeout".into())),
            )],
            skipped,
            &[restriction(false)],
            &[TokenError {
                symbol: "USDT".into(),
                error: "boom".into(),
            }],
        );
        assert_eq!(out.verdict, Verdict::Unknown);
        let statuses: Vec<SourceStatus> = out.sources.iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            [
                SourceStatus::Error,
                SourceStatus::Skipped,
                SourceStatus::Clear,
                SourceStatus::Error
            ]
        );
        let clear = verdict(subject(), vec![screen("trm", false)], vec![], &[], &[]);
        assert_eq!(clear.verdict, Verdict::Clear);
    }
}
