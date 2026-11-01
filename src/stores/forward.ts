import { create } from "zustand";

import type { ForwardInfo } from "@/shared/ipc";

interface ForwardStoreState {
  forwards: ForwardInfo[];
  setForwards: (f: ForwardInfo[]) => void;
  upsertForward: (f: ForwardInfo) => void;
  removeForward: (id: string) => void;
}

export const useForwardStore = create<ForwardStoreState>((set) => ({
  forwards: [],
  setForwards: (forwards) => set({ forwards }),
  upsertForward: (f) =>
    set((s) => {
      const idx = s.forwards.findIndex((x) => x.id === f.id);
      if (idx === -1) return { forwards: [...s.forwards, f] };
      const forwards = [...s.forwards];
      forwards[idx] = f;
      return { forwards };
    }),
  removeForward: (id) => set((s) => ({ forwards: s.forwards.filter((f) => f.id !== id) })),
}));
