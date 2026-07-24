import { Routes, Route } from "react-router-dom";

import { AppShell } from "./components/AppShell";
import { ClusterPage } from "./routes/ClusterPage";
import { LabelingPage } from "./routes/LabelingPage";
import { PersonPage } from "./routes/PersonPage";

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route path="/" element={<LabelingPage />} />
        <Route path="/cluster/:id" element={<ClusterPage />} />
        <Route path="/person/:name" element={<PersonPage />} />
      </Route>
    </Routes>
  );
}
