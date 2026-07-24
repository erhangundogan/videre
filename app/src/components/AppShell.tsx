import { Outlet } from "react-router-dom";

import { ApplicationShell4 } from "@/components/application-shell4";

export function AppShell() {
  return (
    <ApplicationShell4>
      <Outlet />
    </ApplicationShell4>
  );
}
