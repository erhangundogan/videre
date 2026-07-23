import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type FacesData = {
  people: { label: string; representative_id: number }[];
  clusters: { cluster_id: number; face_ids: number[] }[];
  singletons: { face_id: number; hash: string }[];
};

export default function App() {
  const [data, setData] = useState<FacesData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<FacesData>("faces_list").then(setData).catch((e) => setError(String(e)));
  }, []);

  if (error) return <pre style={{ color: "crimson", padding: 16 }}>Error: {error}</pre>;
  if (!data) return <p style={{ padding: 16 }}>Loading…</p>;

  const firstFace =
    data.people[0]?.representative_id ??
    data.clusters[0]?.face_ids[0] ??
    data.singletons[0]?.face_id;

  return (
    <main style={{ padding: 16, fontFamily: "sans-serif" }}>
      <h1>videre (smoke test)</h1>
      <p>
        {data.people.length} people · {data.clusters.length} clusters ·{" "}
        {data.singletons.length} singletons
      </p>
      {firstFace != null && (
        <img
          src={`videre-face://${firstFace}`}
          width={140}
          height={140}
          alt={`face ${firstFace}`}
          style={{ borderRadius: 8, background: "#ddd" }}
        />
      )}
    </main>
  );
}
