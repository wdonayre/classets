import { useCallback, useEffect, useRef, useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { api, ago, bytes, type Browse, type Folder, type PendingPairing, type UiState } from './api';
import './App.css';

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

function Brand() {
    return (
        <header className="brand">
            <Mark />
            <span className="word">Classets</span>
        </header>
    );
}

// ---- Setup + pairing ------------------------------------------------------------------------

function Setup({ s, onDone }: { s: UiState; onDone: () => void }) {
    const [server, setServer] = useState(s.server_url ?? 'http://127.0.0.1:8000');
    const [name, setName] = useState(s.device_name ?? s.default_device_name);
    const [root, setRoot] = useState(s.sync_root ?? s.default_sync_root);
    const [error, setError] = useState('');
    const [busy, setBusy] = useState(false);

    return (
        <div className="center">
            <section className="panel narrow">
                <Brand />
                <div>
                    <div className="eyebrow">Step 1 of 2</div>
                    <h1>Sign in this {s.mobile ? 'phone' : 'device'}</h1>
                    <p className="sub">Your browser opens the Classets sign-in. Your password is never typed into this app.</p>
                </div>
                <form
                    className="stack"
                    onSubmit={async (e) => {
                        e.preventDefault();
                        setBusy(true);
                        setError('');
                        try {
                            await api.configure(server, name, root);
                            onDone();
                        } catch (err) {
                            setError(String(err));
                        } finally {
                            setBusy(false);
                        }
                    }}
                >
                    <label className="lbl" htmlFor="server">Classets server<input id="server" className="inp mono" value={server} onChange={(e) => setServer(e.target.value)} autoCapitalize="off" autoCorrect="off" spellCheck={false} /></label>
                    <label className="lbl" htmlFor="name">Name of this {s.mobile ? 'phone' : 'device'}<input id="name" className="inp" value={name} onChange={(e) => setName(e.target.value)} autoCorrect="off" spellCheck={false} /></label>
                    {!s.mobile && (
                        <label className="lbl" htmlFor="root">
                            Sync folder on this computer
                            <input id="root" className="inp mono" value={root} onChange={(e) => setRoot(e.target.value)} autoCapitalize="off" autoCorrect="off" spellCheck={false} />
                        </label>
                    )}
                    {error && <p className="err">{error}</p>}
                    <button className="btn btn-p btn-lg" type="submit" disabled={busy || !server || !name}>Continue</button>
                </form>
            </section>
        </div>
    );
}

function Pair({ s, onPaired, onBack }: { s: UiState; onPaired: () => void; onBack: () => void }) {
    const [code, setCode] = useState<{ user_code: string; verification_url: string; interval: number } | null>(null);
    const [msg, setMsg] = useState('');
    const [loading, setLoading] = useState(true);
    const started = useRef(false);

    const start = useCallback(async () => {
        setMsg('');
        setLoading(true);
        try {
            const c = await api.pairStart();
            setCode(c);
        } catch (e) {
            setMsg(String(e));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        if (!started.current) {
            started.current = true;
            start();
        }
    }, [start]);

    useEffect(() => {
        if (!code) return;
        const t = setInterval(async () => {
            try {
                const r = await api.pairPoll();
                if (r === 'approved') {
                    clearInterval(t);
                    onPaired();
                } else if (r !== 'pending') {
                    clearInterval(t);
                    setCode(null);
                    setMsg(r === 'denied' ? 'The request was denied.' : 'The code expired.');
                }
            } catch (e) {
                setMsg(String(e));
            }
        }, Math.max(2, code.interval) * 1000);
        return () => clearInterval(t);
    }, [code, onPaired]);

    return (
        <div className="center">
            <section className="panel narrow">
                <Brand />
                <div>
                    <div className="eyebrow">Step 2 of 2</div>
                    <h1>Approve this {s.mobile ? 'phone' : 'device'} on the web</h1>
                    <p className="sub">An owner or admin confirms this code at {s.server_url}/approve.</p>
                </div>
                {code ? (
                    <>
                        <div className="code" aria-label={`Code ${code.user_code}`}>{code.user_code}</div>
                        <p className="sub row"><span className="dot pulse" />Waiting for approval…</p>
                        <button type="button" className="btn btn-p btn-lg" onClick={() => openUrl(code.verification_url)}>Open the approve page</button>
                    </>
                ) : loading ? (
                    <p className="sub row"><span className="dot pulse" />Getting a code…</p>
                ) : (
                    <>
                        {msg && <p className="err">{msg}</p>}
                        <button type="button" className="btn btn-p btn-lg" onClick={start}>Get a new code</button>
                    </>
                )}
                <button type="button" className="btn" onClick={onBack}>Change server or name</button>
                <p className="sup">This {s.mobile ? 'phone' : 'computer'} made its own key. The private half never leaves it.</p>
            </section>
        </div>
    );
}

/** Signing out means pairing again, so it takes a second tap. */
function SignOutButton({ label, onDone }: { label: string; onDone: () => void }) {
    const [armed, setArmed] = useState(false);
    useEffect(() => {
        if (!armed) return;
        const t = setTimeout(() => setArmed(false), 5000);
        return () => clearTimeout(t);
    }, [armed]);
    return (
        <button
            type="button"
            className="btn btn-d"
            onClick={async () => {
                if (!armed) return setArmed(true);
                await api.signOut();
                onDone();
            }}
        >
            {armed ? 'Tap again to sign out. You will need to pair again.' : label}
        </button>
    );
}

/** Plain words for engine errors a person can act on. */
function friendlyError(e: string): string {
    if (/unauthori[sz]ed/i.test(e)) return 'The server no longer accepts this device. Sign it out under Settings and pair it again.';
    if (/error sending request|connect|dns|timed out/i.test(e)) return "Can't reach the Classets server. Files will sync when it's back.";
    return e;
}

// ---- Desktop ----------------------------------------------------------------------------------

function Desktop({ s, refresh }: { s: UiState; refresh: () => void }) {
    const [tab, setTab] = useState<'status' | 'folders' | 'settings'>('status');
    const [folders, setFolders] = useState<Folder[] | null>(null);
    const [busy, setBusy] = useState(false);
    const report = s.status?.last_report;

    useEffect(() => {
        if (tab === 'folders') api.folders().then(setFolders).catch(() => setFolders([]));
    }, [tab]);

    const state = s.paused ? 'Paused' : s.status?.last_error ? 'Needs attention' : s.syncing ? 'Up to date' : 'Not syncing';

    return (
        <div className="shell">
            <nav className="side" aria-label="App">
                <Brand />
                {(['status', 'folders', 'settings'] as const).map((t) => (
                    <button key={t} type="button" className={`nav ${tab === t ? 'on' : ''}`} onClick={() => setTab(t)}>
                        {t === 'status' ? 'Status' : t === 'folders' ? 'Folders' : 'Settings'}
                    </button>
                ))}
                <div className="card side-card">
                    <div className="h3">{s.device_name}</div>
                    <div className="sup">{s.me?.user.name} · {s.me?.workspace.name}</div>
                    <button type="button" className="link" onClick={() => s.server_url && openUrl(`${s.server_url}/files`)}>Open web app</button>
                </div>
            </nav>
            <main className="panel grow">
                {tab === 'status' && (
                    <>
                        <div className="card row hero">
                            <span className={`dot big ${s.paused ? 'warn' : s.status?.last_error ? 'err' : 'ok'}`} />
                            <div style={{ flex: 1 }}>
                                <h1>{state}</h1>
                                <div className="sup">Last sync {ago(s.status?.last_sync)} · {report ? `${report.uploaded} up, ${report.downloaded} down` : 'no passes yet'}</div>
                            </div>
                            <button type="button" className="btn" disabled={busy} onClick={async () => { setBusy(true); await api.syncNow().catch(() => undefined); setBusy(false); refresh(); }}>
                                {busy ? 'Syncing…' : 'Sync now'}
                            </button>
                            <button type="button" className="btn" onClick={async () => { await api.setPaused(!s.paused); refresh(); }}>{s.paused ? 'Resume' : 'Pause'}</button>
                        </div>
                        {s.status?.last_error && <p className="err">{friendlyError(s.status.last_error)}</p>}
                        <h2>Recent activity</h2>
                        <div className="card list">
                            {s.log.length === 0 && <div className="list-row sub">Nothing yet. Files you put in {s.sync_root} sync automatically.</div>}
                            {s.log.map((l, i) => (
                                <div key={i} className="list-row">
                                    <span className={`dot ${l.ok ? 'ok' : 'err'}`} />
                                    <span style={{ flex: 1 }}>{l.text}</span>
                                    <span className="sup">{ago(l.at)}</span>
                                </div>
                            ))}
                        </div>
                        <p className="sup">Local folder <span className="mono">{s.sync_root}</span></p>
                    </>
                )}
                {tab === 'folders' && (
                    <>
                        <h1>Folders on this computer</h1>
                        <p className="sub">Only what your admin allows. Sync rules for each folder are changed on the web, under Devices.</p>
                        <div className="card list">
                            {folders === null && <div className="list-row sub">Loading…</div>}
                            {folders?.length === 0 && <div className="list-row sub">No folders are shared with this device yet.</div>}
                            {folders?.map((f) => (
                                <div key={f.id} className="list-row">
                                    <span style={{ flex: 1 }}>
                                        <span className="h3">{f.path}</span>
                                        <span className="sup block">{f.access === 'edit' ? 'Can edit' : 'View only (download only)'}</span>
                                    </span>
                                    <span className={`badge ${f.mode === 'sync' ? 'b-ok' : 'b-mute'}`}>{f.mode === 'sync' ? 'Syncs' : f.mode === 'on_demand' ? 'On demand' : "Doesn't sync"}</span>
                                </div>
                            ))}
                        </div>
                    </>
                )}
                {tab === 'settings' && (
                    <>
                        <h1>Settings</h1>
                        <div className="card list">
                            <div className="list-row"><span className="sub" style={{ width: 140 }}>Server</span><span className="mono">{s.server_url}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 140 }}>Device name</span><span>{s.device_name}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 140 }}>Sync folder</span><span className="mono">{s.sync_root}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 140 }}>Device ID</span><span className="mono">{s.status?.device_id}</span></div>
                        </div>
                        <p className="sup">Closing the window keeps Classets syncing in the tray. Use the tray menu to quit.</p>
                        <SignOutButton label="Sign this device out" onDone={refresh} />
                    </>
                )}
            </main>
        </div>
    );
}

// ---- Mobile companion ------------------------------------------------------------------------------

function Mobile({ s, refresh }: { s: UiState; refresh: () => void }) {
    const [tab, setTab] = useState<'files' | 'approve' | 'account'>('files');
    const [path, setPath] = useState('');
    const [data, setData] = useState<Browse | null>(null);
    const [pending, setPending] = useState<PendingPairing[]>([]);
    const [typed, setTyped] = useState('');
    const [found, setFound] = useState<PendingPairing | null>(null);
    const [codeMsg, setCodeMsg] = useState('');
    const [msg, setMsg] = useState('');
    const input = useRef<HTMLInputElement>(null);
    const canApprove = s.me?.role === 'owner' || s.me?.role === 'admin';

    const load = useCallback(async (p: string) => {
        setMsg('');
        try {
            setData(await api.browse(p));
        } catch (e) {
            setMsg(String(e));
        }
    }, []);

    useEffect(() => {
        if (tab === 'files') load(path);
        if (tab === 'approve') api.pending().then(setPending).catch((e) => setMsg(String(e)));
    }, [tab, path, load]);

    async function upload(files: FileList) {
        for (const f of Array.from(files)) {
            setMsg(`Uploading ${f.name}…`);
            try {
                const buf = new Uint8Array(await f.arrayBuffer());
                await api.uploadBytes(path, f.name, Array.from(buf));
            } catch (e) {
                setMsg(String(e));
                return;
            }
        }
        setMsg('Uploaded.');
        load(path);
    }

    const parent = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';

    return (
        <div className="mobile">
            <header className="mtop">
                <Mark size={24} />
                <span className="word">Classets</span>
                <span className="sup" style={{ marginLeft: 'auto' }}>{s.me?.workspace.name}</span>
            </header>
            <main className="mmain">
                {msg && <p className={msg.startsWith('Upload') && !msg.includes('rror') ? 'sub' : 'err'}>{msg}</p>}
                {tab === 'files' && (
                    <>
                        <div className="row">
                            {path && <button type="button" className="btn" onClick={() => setPath(parent)}>Back</button>}
                            <h1 style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>{path ? path.split('/').pop() : 'All files'}</h1>
                            {data?.can_upload && (
                                <>
                                    <input ref={input} type="file" multiple hidden accept="*/*" onChange={(e) => e.target.files && upload(e.target.files)} />
                                    <button type="button" className="btn btn-p" onClick={() => input.current?.click()}>Upload</button>
                                </>
                            )}
                        </div>
                        <div className="card list">
                            {data?.folders.map((f) => (
                                <button key={f.path} type="button" className="list-row tap" onClick={() => setPath(f.path)}>
                                    <span className="ficon" aria-hidden="true">
                                        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>
                                    </span>
                                    <span style={{ flex: 1, textAlign: 'left' }}>{f.name}</span>
                                </button>
                            ))}
                            {data?.files.map((f) => (
                                <button
                                    key={f.path}
                                    type="button"
                                    className="list-row tap"
                                    onClick={async () => f.version_id && openUrl(await api.downloadLink(f.version_id))}
                                >
                                    {f.thumb_url ? <img className="thumb" src={f.thumb_url} alt="" /> : <span className="ficon mono">{f.name.split('.').pop()?.slice(0, 4).toUpperCase()}</span>}
                                    <span style={{ flex: 1, textAlign: 'left', minWidth: 0 }}>
                                        <span className="h3 ellipsis">{f.name}</span>
                                        <span className="sup block">{bytes(f.size)} · {ago(f.modified_at)}</span>
                                    </span>
                                </button>
                            ))}
                            {data && data.folders.length === 0 && data.files.length === 0 && <div className="list-row sub">Nothing here yet.</div>}
                        </div>
                    </>
                )}
                {tab === 'approve' && (
                    <>
                        <h1>Devices waiting</h1>
                        {!canApprove && <p className="sub">Only owners and admins can approve devices.</p>}
                        <div className="card list">
                            {pending.length === 0 && <div className="list-row sub">No devices are waiting for approval.</div>}
                            {pending.map((p) => (
                                <div key={p.user_code} className="list-row" style={{ flexWrap: 'wrap' }}>
                                    <span style={{ flex: '1 1 160px' }}>
                                        <span className="h3">{p.name}</span>
                                        <span className="sup block">{p.requested_by} · code <span className="mono">{p.user_code}</span></span>
                                    </span>
                                    <button type="button" className="btn btn-p" onClick={async () => { await api.decide(p.user_code, true); setPending(await api.pending()); }}>Approve</button>
                                    <button type="button" className="btn" onClick={async () => { await api.decide(p.user_code, false); setPending(await api.pending()); }}>Deny</button>
                                </div>
                            ))}
                        </div>
                        <p className="sup">Devices that members ask for show up here.</p>
                        {canApprove && (
                            <>
                                <h2>Approve a code</h2>
                                <p className="sub">Type the code a new device is showing. Only approve if that device is in front of you.</p>
                                {!found ? (
                                    <form
                                        className="row"
                                        onSubmit={async (e) => {
                                            e.preventDefault();
                                            setCodeMsg('');
                                            try {
                                                setFound(await api.lookup(typed.trim()));
                                            } catch {
                                                setCodeMsg('No device is waiting with that code. Check it, or get a new one on the device.');
                                            }
                                        }}
                                    >
                                        <input className="inp mono" style={{ flex: 1, textTransform: 'uppercase' }} placeholder="KQ7M-4TZP" value={typed}
                                            onChange={(e) => setTyped(e.target.value)} autoCapitalize="characters" autoCorrect="off" spellCheck={false} aria-label="Code on the device" />
                                        <button type="submit" className="btn btn-p" disabled={typed.trim().length < 8}>Check</button>
                                    </form>
                                ) : (
                                    <div className="card list">
                                        <div className="list-row" style={{ flexWrap: 'wrap' }}>
                                            <span style={{ flex: '1 1 160px' }}>
                                                <span className="h3">{found.name}</span>
                                                <span className="sup block">{found.os} · code <span className="mono">{found.user_code}</span></span>
                                            </span>
                                            <button type="button" className="btn btn-p" onClick={async () => {
                                                try { await api.decide(found.user_code, true); setCodeMsg(`${found.name} is approved.`); }
                                                catch (e) { setCodeMsg(String(e)); }
                                                setFound(null); setTyped('');
                                            }}>Approve</button>
                                            <button type="button" className="btn" onClick={async () => {
                                                await api.decide(found.user_code, false).catch(() => undefined);
                                                setCodeMsg(`${found.name} was denied.`); setFound(null); setTyped('');
                                            }}>Deny</button>
                                        </div>
                                    </div>
                                )}
                                {codeMsg && <p className="sub">{codeMsg}</p>}
                            </>
                        )}
                    </>
                )}
                {tab === 'account' && (
                    <>
                        <h1>Account</h1>
                        <div className="card list">
                            <div className="list-row"><span className="sub" style={{ width: 110 }}>Signed in as</span><span>{s.me?.user.name}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 110 }}>Workspace</span><span>{s.me?.workspace.name}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 110 }}>This phone</span><span>{s.device_name}</span></div>
                            <div className="list-row"><span className="sub" style={{ width: 110 }}>Server</span><span className="mono ellipsis">{s.server_url}</span></div>
                        </div>
                        <button type="button" className="btn" onClick={() => s.server_url && openUrl(`${s.server_url}/files`)}>Open the web app</button>
                        <SignOutButton label="Sign this phone out" onDone={refresh} />
                    </>
                )}
            </main>
            <nav className="mtabs" aria-label="Sections">
                {(['files', 'approve', 'account'] as const).map((t) => (
                    <button key={t} type="button" className={tab === t ? 'on' : ''} onClick={() => setTab(t)}>
                        {t === 'files' ? 'Files' : t === 'approve' ? 'Approvals' : 'Account'}
                    </button>
                ))}
            </nav>
        </div>
    );
}

// ---- Root -----------------------------------------------------------------------------------------

export default function App() {
    const [s, setS] = useState<UiState | null>(null);
    const [editing, setEditing] = useState(false);

    const refresh = useCallback(() => {
        api.state().then(setS).catch(() => undefined);
    }, []);

    useEffect(() => {
        refresh();
        const t = setInterval(refresh, 2000);
        return () => clearInterval(t);
    }, [refresh]);

    if (!s) return <div className="center sub">Starting…</div>;
    if (s.starting) {
        return (
            <div className="center">
                <section className="panel narrow">
                    <Brand />
                    <p className="sub row"><span className="dot pulse" />Opening this {s.mobile ? 'phone' : 'device'}'s saved key…</p>
                    <p className="sup">If your computer asks, allow Classets to use its saved item in the keychain.</p>
                </section>
            </div>
        );
    }
    if (!s.configured || editing) return <Setup s={s} onDone={() => { setEditing(false); refresh(); }} />;
    if (!s.paired) return <Pair s={s} onPaired={refresh} onBack={() => setEditing(true)} />;
    return s.mobile ? <Mobile s={s} refresh={refresh} /> : <Desktop s={s} refresh={refresh} />;
}
