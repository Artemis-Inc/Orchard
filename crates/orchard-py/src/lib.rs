//! Python bindings for Orchard 3.0 (PyO3). A thin adapter over the `orchard`
//! facade: `check`, `compile`, and an `Agent`/`Session` that runs offline (or
//! against any provider configured in the `.orch` file).
//!
//! Build with maturin: `maturin develop -m crates/orchard-py/Cargo.toml`.

use ::orchard::{Agent as CoreAgent, Runtime, Session as CoreSession};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// Static analysis → rendered diagnostics (empty string if clean).
#[pyfunction]
fn check(source: &str, filename: &str) -> String {
    let diags = CoreAgent::check(source, filename);
    diags
        .iter()
        .map(|d| d.render(source))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Lower to the JSON IR (raises on diagnostics).
#[pyfunction]
fn compile(source: &str, filename: &str) -> PyResult<String> {
    CoreAgent::compile(source, filename).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// A loaded Orchard agent.
#[pyclass]
struct Agent {
    inner: CoreAgent,
}

#[pymethods]
impl Agent {
    /// Load + check + lower from source.
    #[staticmethod]
    fn load(source: &str, filename: &str) -> PyResult<Agent> {
        CoreAgent::load(source, filename)
            .map(|inner| Agent { inner })
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    #[getter]
    fn name(&self) -> String {
        self.inner.name().to_string()
    }

    /// Build a session (default mock/in-memory unless the file names a provider).
    #[pyo3(signature = (base_dir=None))]
    fn session(&self, base_dir: Option<String>) -> PyResult<Session> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let session = Runtime::builder(self.inner.clone())
            .base_dir(base_dir.unwrap_or_else(|| ".".into()))
            .build()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Session { rt, session })
    }
}

/// A live agent session.
#[pyclass]
struct Session {
    rt: tokio::runtime::Runtime,
    session: CoreSession,
}

#[pymethods]
impl Session {
    /// Drive one `on message` turn (releases the GIL while running).
    fn message(&self, py: Python<'_>, text: &str) -> PyResult<String> {
        py.allow_threads(|| self.rt.block_on(self.session.message(text)))
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// A one-shot task (alias for `message`).
    fn task(&self, py: Python<'_>, text: &str) -> PyResult<String> {
        self.message(py, text)
    }
}

#[pymodule]
fn orchard(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", ::orchard::VERSION)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_class::<Agent>()?;
    m.add_class::<Session>()?;
    Ok(())
}
