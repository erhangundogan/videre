import { useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useClient } from "@/lib/ClientProvider";
import { useClusterDetail, useFacesList, useMutations } from "@/lib/queries";
import { FaceImage } from "@/components/FaceImage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "@/components/ui/dialog";
import type { ClusterFaceData } from "@/lib/models";

const MAX_NAME_LEN = 60;

// Trim, collapse internal whitespace, strip control/bidi-spoofing characters,
// and cap length by code point - mirrors sanitizeName() in CLUSTER_HTML; the
// backend also sanitizes.
function sanitizeName(raw: string): string {
  const filtered = Array.from(raw)
    .filter((ch) => {
      const cp = ch.codePointAt(0)!;
      if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
      if (cp === 0x200b) return false;
      if (cp === 0x200e || cp === 0x200f) return false;
      if (cp >= 0x202a && cp <= 0x202e) return false;
      if (cp >= 0x2060 && cp <= 0x2069) return false;
      if (cp === 0xfeff) return false;
      return true;
    })
    .join("");
  const collapsed = filtered.trim().replace(/\s+/g, " ");
  return Array.from(collapsed).slice(0, MAX_NAME_LEN).join("");
}

function basename(p: string): string {
  return p.split("/").pop() || p;
}

function FaceCard({
  face,
  onRemove,
  onAssign,
  onOpen,
  peopleNames,
}: {
  face: ClusterFaceData;
  onRemove: (faceId: number) => void;
  onAssign: (faceId: number, label: string) => void;
  onOpen: (faceId: number) => void;
  peopleNames: string[];
}) {
  const [assigning, setAssigning] = useState(false);
  const [value, setValue] = useState("");
  const listId = `people-list-${face.face_id}`;

  const submit = () => {
    const label = sanitizeName(value);
    if (!label) return;
    onAssign(face.face_id, label);
    setAssigning(false);
    setValue("");
  };

  return (
    <div className="w-[200px] rounded-lg border bg-card p-2.5">
      <button
        type="button"
        className="mb-1.5 block w-full cursor-pointer"
        onClick={() => onOpen(face.face_id)}
        title="View full size"
      >
        <FaceImage faceId={face.face_id} size={180} className="w-full" />
      </button>
      <div className="truncate text-[11px] text-muted-foreground" title={face.path}>
        {basename(face.path)}
      </div>
      <div className="mt-0.5 text-[11px] text-muted-foreground/70">#{face.face_id}</div>
      {assigning ? (
        <div className="mt-2 flex flex-col gap-1.5">
          <Input
            autoFocus
            list={listId}
            placeholder="Person name"
            maxLength={MAX_NAME_LEN}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                submit();
              }
              if (e.key === "Escape") {
                setAssigning(false);
                setValue("");
              }
            }}
          />
          <datalist id={listId}>
            {peopleNames.map((n) => (
              <option key={n} value={n} />
            ))}
          </datalist>
          <div className="flex gap-1">
            <Button size="sm" className="flex-1" onClick={submit}>
              Assign
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="flex-1"
              onClick={() => {
                setAssigning(false);
                setValue("");
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <div className="mt-2 flex flex-wrap gap-1.5">
          <Button
            variant="outline"
            size="sm"
            className="border-red-300 text-red-600 hover:bg-red-50 hover:text-red-700 dark:border-red-900 dark:hover:bg-red-950/30"
            onClick={() => onRemove(face.face_id)}
          >
            Remove
          </Button>
          <Button variant="outline" size="sm" onClick={() => setAssigning(true)}>
            Assign
          </Button>
        </div>
      )}
    </div>
  );
}

export function ClusterPage() {
  const params = useParams<{ id: string }>();
  const clusterId = Number(params.id);
  const navigate = useNavigate();
  const client = useClient();

  const { data, isLoading, error } = useClusterDetail(clusterId);
  const { data: facesList } = useFacesList();
  const { removeFace, newPerson, dissolveCluster } = useMutations();

  const [assignAllValue, setAssignAllValue] = useState("");
  const [lightboxFaceId, setLightboxFaceId] = useState<number | null>(null);

  const peopleNames = useMemo(
    () => (facesList ? facesList.people.map((p) => p.label) : []),
    [facesList]
  );

  const doAssignAll = () => {
    if (!data) return;
    const label = sanitizeName(assignAllValue);
    if (!label) return;
    const faceIds = data.faces.map((f) => f.face_id);
    newPerson.mutate(
      { faceIds, label },
      { onSuccess: () => navigate("/") }
    );
  };

  const doDissolve = () => {
    if (!data) return;
    if (
      !confirm(
        `Dissolve cluster ${clusterId}? Its ${data.faces.length} face(s) will become unassigned singletons (not deleted).`
      )
    )
      return;
    dissolveCluster.mutate(clusterId, { onSuccess: () => navigate("/") });
  };

  if (Number.isNaN(clusterId)) {
    return <div className="p-4 text-sm text-destructive">Invalid cluster id.</div>;
  }
  if (isLoading) {
    return <div className="p-4 text-sm text-muted-foreground">Loading...</div>;
  }
  if (error) {
    return (
      <div className="p-4 text-sm text-destructive">
        Error loading: {(error as Error).message ?? String(error)}
      </div>
    );
  }
  if (!data) return null;

  return (
    <div className="p-4">
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <a href="/" onClick={(e) => { e.preventDefault(); navigate("/"); }} className="text-sm text-blue-600 hover:underline dark:text-blue-400">
          &larr; Back to labeling
        </a>
        <strong>Cluster {clusterId}</strong>
        <span className="text-sm text-muted-foreground">{data.faces.length} face(s)</span>
      </div>

      <div className="mb-4 flex flex-wrap items-center gap-2 rounded-lg border bg-card p-3">
        <strong className="text-sm">Assign all to:</strong>
        <Input
          list="people-list-all"
          placeholder="Person name"
          maxLength={MAX_NAME_LEN}
          value={assignAllValue}
          onChange={(e) => setAssignAllValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              doAssignAll();
            }
          }}
          className="w-40"
        />
        <datalist id="people-list-all">
          {peopleNames.map((n) => (
            <option key={n} value={n} />
          ))}
        </datalist>
        <Button size="sm" onClick={doAssignAll}>
          Assign cluster
        </Button>
        <Button
          variant="outline"
          size="sm"
          className="ml-auto border-red-300 text-red-600 hover:bg-red-50 hover:text-red-700 dark:border-red-900 dark:hover:bg-red-950/30"
          onClick={doDissolve}
        >
          Dissolve cluster (wrong grouping)
        </Button>
      </div>

      <div className="grid grid-cols-[repeat(auto-fill,200px)] gap-3.5">
        {data.faces.map((f) => (
          <FaceCard
            key={f.face_id}
            face={f}
            onRemove={(faceId) => removeFace.mutate(faceId)}
            onAssign={(faceId, label) => newPerson.mutate({ faceIds: [faceId], label })}
            onOpen={setLightboxFaceId}
            peopleNames={peopleNames}
          />
        ))}
      </div>

      <Dialog open={lightboxFaceId !== null} onOpenChange={(open) => !open && setLightboxFaceId(null)}>
        <DialogContent className="max-w-3xl">
          <DialogTitle>Face #{lightboxFaceId}</DialogTitle>
          {lightboxFaceId !== null && (
            <img
              src={client.originalImageUrl(lightboxFaceId)}
              alt={`face ${lightboxFaceId} original`}
              className="max-h-[75vh] w-full rounded object-contain"
            />
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}
