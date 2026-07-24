import { createContext, useContext, type ReactNode } from "react";
import type { VidereClient } from "./client";
const Ctx = createContext<VidereClient | null>(null);
export function ClientProvider({ client, children }: { client: VidereClient; children: ReactNode }) {
  return <Ctx.Provider value={client}>{children}</Ctx.Provider>;
}
export function useClient(): VidereClient {
  const c = useContext(Ctx);
  if (!c) throw new Error("useClient must be used within ClientProvider");
  return c;
}
