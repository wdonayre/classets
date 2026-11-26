import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function Mark({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
      <rect width="64" height="64" rx="16" className="mark-tile" />
      <path d="M39.62 20.97A14 14 0 1 0 39.62 43.03" className="mark-c" />
      <circle cx="39.62" cy="20.97" r="5.25" className="mark-dot" />
      <circle cx="39.62" cy="43.03" r="5.25" className="mark-dot" />
    </svg>
  );
}

function App() {
  const [engine, setEngine] = useState<string | null>(null);

  useEffect(() => {
    invoke<string>("engine_version")
      .then(setEngine)
      .catch(() => setEngine(null));
  }, []);

  return (
    <div className="app">
      <header className="brand">
        <Mark />
        <span className="word">CLASSETS</span>
      </header>
      <main className="panel">
        <h1>Not signed in yet</h1>
        <p className="sub">
          Sign this device in with a pairing code. An owner or admin approves it on the web,
          then CLASSETS starts syncing the folders you're allowed to have here.
        </p>
        <button type="button" className="btn-primary" disabled>
          Sign in this device
        </button>
        <p className="sup">Pairing arrives in Sprint 3. This build only checks the app starts.</p>
      </main>
      <footer className="sup">
        Sync engine {engine ? `v${engine}` : "not reachable"}
      </footer>
    </div>
  );
}

export default App;
