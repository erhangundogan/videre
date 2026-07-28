import { Outlet } from "react-router-dom";

import { ApplicationShell11 } from "@/components/application-shell11";

export function AppShell() {
  return (
    <ApplicationShell11>
      <Outlet />
    </ApplicationShell11>
  );
}
