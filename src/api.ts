import { invoke } from '@tauri-apps/api/core';

export interface SyncReport { uploaded: number; downloaded: number; deleted: number; conflicts: number; errors: string[] }
export interface EngineStatus { paired: boolean; device_id: string | null; last_sync: string | null; last_report: SyncReport | null; last_error: string | null }
export interface Me { device: { id: string; name: string; status: string; kind: string }; user: { id: number; name: string; email: string }; workspace: { id: number; name: string }; role: string }
export interface UiState {
    configured: boolean; paired: boolean; syncing: boolean; paused: boolean; os: string; mobile: boolean;
    server_url: string | null; device_name: string | null; sync_root: string | null;
    default_device_name: string; default_sync_root: string;
    status: EngineStatus | null; me: Me | null; log: { at: string; text: string; ok: boolean }[];
}
export interface Folder { id: number; path: string; access: 'edit' | 'view'; travel: string; mode: 'sync' | 'on_demand' | 'off' }
export interface Browse {
    path: string; can_upload: boolean;
    folders: { name: string; path: string }[];
    files: { name: string; path: string; size: number; version_id: number | null; modified_at: string | null; thumb_url: string | null }[];
}
export interface PendingPairing { user_code: string; name: string; os: string; kind: string; requested_by?: string; created_at?: string }

export const api = {
    state: () => invoke<UiState>('app_state'),
    configure: (serverUrl: string, deviceName: string, syncRoot: string) =>
        invoke<void>('configure', { serverUrl, deviceName, syncRoot }),
    pairStart: () => invoke<{ user_code: string; verification_url: string; expires_in: number; interval: number }>('pair_start'),
    pairPoll: () => invoke<string>('pair_poll'),
    syncNow: () => invoke<SyncReport>('sync_now'),
    setPaused: (paused: boolean) => invoke<void>('set_paused', { paused }),
    folders: () => invoke<Folder[]>('folders'),
    browse: (path: string) => invoke<Browse>('browse', { path }),
    downloadLink: (versionId: number) => invoke<string>('download_link', { versionId }),
    uploadBytes: (folder: string, name: string, bytes: number[]) => invoke<string>('upload_bytes', { folder, name, bytes }),
    pending: () => invoke<PendingPairing[]>('pending_pairings'),
    decide: (code: string, approve: boolean) => invoke<void>('decide_pairing', { code, approve }),
    signOut: () => invoke<void>('sign_out'),
};

export function bytes(n?: number | null): string {
    if (n === null || n === undefined) return '—';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let v = n;
    while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
    return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}

export function ago(iso?: string | null): string {
    if (!iso) return 'never';
    const s = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
    if (s < 45) return 'just now';
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    if (s < 86400) return `${Math.round(s / 3600)} h ago`;
    return new Date(iso).toLocaleDateString();
}
