//! The policy engine: budgets, spend accounting, circuit breaker. Per-turn
//! counters (steps/requests/tool_calls) reset each turn; spend + breaker
//! accumulate per session. Thread-safe for concurrency (P9) via a single Mutex
//! and atomic check-and-debit in [`PolicyEngine::check_step`].

use crate::error::HostError;
use crate::pricing;
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const UNATTENDED_DEFAULT_SPEND: f64 = 5.00;
pub const TOOL_FAILURE_BREAKER: u32 = 3;

/// One active `budget`/`delegate` scope's caps. The effective cap at any moment
/// is the min over the base caps and all active frames — see [`Inner::eff_*`].
#[derive(Clone, Copy)]
struct BudgetFrame {
    steps: Option<i64>,
    tool_calls: Option<i64>,
    spend: Option<f64>,
}

struct Inner {
    // base caps (immutable after construction)
    max_steps: i64,
    max_tool_calls: i64,
    max_requests: i64,
    spend_cap: Option<f64>,
    /// Active budget-scope frames keyed by token. A set (not a stack) so that
    /// concurrent `parallel` branches entering/exiting scopes in any order can
    /// never corrupt each other's caps; the cap is always the min over frames.
    frames: HashMap<u64, BudgetFrame>,
    // per-turn counters
    steps: i64,
    requests: i64,
    tool_calls: i64,
    // per-session
    spend: f64,
    on_violation: String,
    failures: HashMap<String, u32>,
    broken: HashSet<String>,
}

impl Inner {
    fn eff_max_steps(&self) -> i64 {
        self.frames
            .values()
            .filter_map(|f| f.steps)
            .fold(self.max_steps, i64::min)
    }
    fn eff_max_tool_calls(&self) -> i64 {
        self.frames
            .values()
            .filter_map(|f| f.tool_calls)
            .fold(self.max_tool_calls, i64::min)
    }
    fn eff_spend_cap(&self) -> Option<f64> {
        self.frames
            .values()
            .filter_map(|f| f.spend)
            .fold(self.spend_cap, |acc, s| Some(acc.map_or(s, |c| c.min(s))))
    }
}

pub struct PolicyEngine {
    inner: Mutex<Inner>,
    /// Per-model pricing override (`(input, output)` $/Mtok), if the manifest set one.
    pricing_override: Option<(f64, f64)>,
    /// Monotonic source of budget-frame tokens.
    next_token: AtomicU64,
}

/// An opaque token identifying an active `budget` scope's frame.
#[derive(Clone, Copy)]
pub struct SavedCaps(u64);

impl PolicyEngine {
    /// Build from the manifest `policy` object.
    pub fn from_manifest(policy: &Json, unattended: bool) -> PolicyEngine {
        let geti = |k: &str, d: i64| policy.get(k).and_then(|v| v.as_i64()).unwrap_or(d);
        let mut spend_cap = policy.get("max_spend_usd").and_then(|v| v.as_f64());
        if spend_cap.is_none() && unattended {
            spend_cap = Some(UNATTENDED_DEFAULT_SPEND);
        }
        let on_violation = policy
            .get("on_violation")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();
        let pricing_override = policy.get("pricing").and_then(|p| {
            let i = p.get("input_per_mtok")?.as_f64()?;
            let o = p.get("output_per_mtok")?.as_f64()?;
            Some((i, o))
        });
        PolicyEngine {
            inner: Mutex::new(Inner {
                max_steps: geti("max_steps", 25),
                max_tool_calls: geti("max_tool_calls", 100),
                max_requests: geti("max_requests_per_run", 50),
                spend_cap,
                frames: HashMap::new(),
                steps: 0,
                requests: 0,
                tool_calls: 0,
                spend: 0.0,
                on_violation,
                failures: HashMap::new(),
                broken: HashSet::new(),
            }),
            pricing_override,
            next_token: AtomicU64::new(1),
        }
    }

    /// Reset per-turn counters (spend + breaker persist).
    pub fn begin_turn(&self) {
        let mut g = self.inner.lock().unwrap();
        g.steps = 0;
        g.requests = 0;
        g.tool_calls = 0;
    }

    /// Enforce + increment one model round-trip. Atomic check-and-debit.
    pub fn check_step(&self) -> Result<(), HostError> {
        let mut g = self.inner.lock().unwrap();
        let max_steps = g.eff_max_steps();
        if g.steps >= max_steps {
            return Err(violation(
                format!("max_steps reached ({max_steps})"),
                &g.on_violation,
            ));
        }
        if g.requests >= g.max_requests {
            return Err(violation(
                format!("max_requests_per_run reached ({})", g.max_requests),
                &g.on_violation,
            ));
        }
        if let Some(cap) = g.eff_spend_cap() {
            if g.spend >= cap {
                return Err(violation(
                    format!(
                        "max_spend_usd reached (~${:.4} of ${:.2} cap)",
                        g.spend, cap
                    ),
                    &g.on_violation,
                ));
            }
        }
        g.steps += 1;
        g.requests += 1;
        Ok(())
    }

    pub fn check_tool_call(&self) -> Result<(), HostError> {
        let mut g = self.inner.lock().unwrap();
        let max = g.eff_max_tool_calls();
        if g.tool_calls >= max {
            return Err(violation(
                format!("max_tool_calls reached ({max})"),
                &g.on_violation,
            ));
        }
        g.tool_calls += 1;
        Ok(())
    }

    /// Post-hoc spend accounting. `mock`/`ollama` are free.
    pub fn record_usage(&self, provider: &str, model: &str, input_tokens: i64, output_tokens: i64) {
        if provider == "mock" || provider == "ollama" {
            return;
        }
        if let Some(cost) =
            pricing::cost_usd(model, input_tokens, output_tokens, self.pricing_override)
        {
            self.inner.lock().unwrap().spend += cost;
        }
    }

    // ---- budget scope (min-composition) ----

    pub fn enter_budget(
        &self,
        spend: Option<f64>,
        steps: Option<i64>,
        tool_calls: Option<i64>,
    ) -> SavedCaps {
        let token = self.next_token.fetch_add(1, Ordering::SeqCst);
        self.inner.lock().unwrap().frames.insert(
            token,
            BudgetFrame {
                steps,
                tool_calls,
                spend,
            },
        );
        SavedCaps(token)
    }

    pub fn exit_budget(&self, saved: SavedCaps) {
        self.inner.lock().unwrap().frames.remove(&saved.0);
    }

    // ---- circuit breaker ----

    pub fn breaker_key(name: &str, args: &Json) -> String {
        let canon = serde_json::to_string(args).unwrap_or_default();
        format!("{name}\u{0}{canon}")
    }
    pub fn is_broken(&self, key: &str) -> bool {
        self.inner.lock().unwrap().broken.contains(key)
    }
    pub fn record_tool_failure(&self, key: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        let c = g.failures.entry(key.to_string()).or_insert(0);
        *c += 1;
        if *c >= TOOL_FAILURE_BREAKER {
            g.broken.insert(key.to_string());
            return true;
        }
        false
    }
    pub fn record_tool_success(&self, key: &str) {
        self.inner.lock().unwrap().failures.remove(key);
    }

    pub fn summary(&self) -> String {
        let g = self.inner.lock().unwrap();
        format!(
            "{} steps, {} tool calls, {} requests, ~${:.4} estimated",
            g.steps, g.tool_calls, g.requests, g.spend
        )
    }

    pub fn spend(&self) -> f64 {
        self.inner.lock().unwrap().spend
    }
    pub fn steps(&self) -> i64 {
        self.inner.lock().unwrap().steps
    }
}

fn violation(reason: String, _on_violation: &str) -> HostError {
    // `ask` headroom is interactive (P13); for now all violations stop.
    HostError::Policy(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine() -> PolicyEngine {
        PolicyEngine::from_manifest(&json!({"max_steps": 25, "max_tool_calls": 100}), false)
    }

    #[test]
    fn budget_frames_compose_as_min() {
        let p = engine();
        assert_eq!(p.inner.lock().unwrap().eff_max_steps(), 25);
        let a = p.enter_budget(None, Some(10), None);
        assert_eq!(p.inner.lock().unwrap().eff_max_steps(), 10);
        let b = p.enter_budget(None, Some(5), None);
        assert_eq!(p.inner.lock().unwrap().eff_max_steps(), 5);
        // Exit out of order (as concurrent parallel branches would): removing the
        // tighter frame must restore the looser cap, never corrupt the base.
        p.exit_budget(b);
        assert_eq!(p.inner.lock().unwrap().eff_max_steps(), 10);
        p.exit_budget(a);
        assert_eq!(p.inner.lock().unwrap().eff_max_steps(), 25);
    }

    #[test]
    fn spend_cap_is_min_of_base_and_frames() {
        let p = PolicyEngine::from_manifest(&json!({"max_spend_usd": 2.0}), false);
        assert_eq!(p.inner.lock().unwrap().eff_spend_cap(), Some(2.0));
        let t = p.enter_budget(Some(0.5), None, None);
        assert_eq!(p.inner.lock().unwrap().eff_spend_cap(), Some(0.5));
        p.exit_budget(t);
        assert_eq!(p.inner.lock().unwrap().eff_spend_cap(), Some(2.0));
    }
}
