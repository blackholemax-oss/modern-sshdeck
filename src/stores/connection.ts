import { create } from "zustand";

import type { AppConfig, HostProfile } from "@/shared/ipc";

interface ConnectionState {
  hosts: HostProfile[];
  config: AppConfig | null;
  loading: boolean;

  setHosts: (hosts: HostProfile[]) => void;
  upsertHost: (host: HostProfile) => void;
  removeHost: (id: string) => void;
  setConfig: (config: AppConfig) => void;
  setLoading: (loading: boolean) => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  hosts: [],
  config: null,
  loading: false,

  setHosts: (hosts) => set({ hosts }),
  upsertHost: (host) =>
    set((s) => {
      const idx = s.hosts.findIndex((h) => h.id === host.id);
      if (idx === -1) return { hosts: [...s.hosts, host] };
      const hosts = [...s.hosts];
      hosts[idx] = host;
      return { hosts };
    }),
  removeHost: (id) => set((s) => ({ hosts: s.hosts.filter((h) => h.id !== id) })),
  setConfig: (config) => set({ config }),
  setLoading: (loading) => set({ loading }),
}));
