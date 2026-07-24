import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useClient } from "@/lib/ClientProvider";
import { usePersonDetail, useMutations } from "@/lib/queries";
import { FaceImage } from "@/components/FaceImage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import type { PersonFaceData } from "@/lib/models";

const MAX_NAME_LEN = 60;

// Trim, collapse internal whitespace, strip control/bidi-spoofing characters,
// and cap length by code point - mirrors sanitizeName() in PERSON_HTML; the
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
  onSetDefault,
  onOpen,
}: {
  face: PersonFaceData;
  onRemove: (faceId: number) => void;
  onSetDefault: (faceId: number) => void;
  onOpen: (faceId: number) => void;
}) {
  return (
    <div
      className={
        "relative w-[200px] rounded-lg border bg-card p-2.5" +
        (face.is_primary ? " border-blue-600 ring-2 ring-blue-600" : "")
      }
    >
      {face.is_primary && (
        <span className="absolute left-1.5 top-1.5 z-10 rounded-full bg-blue-600 px-2 py-0.5 text-[11px] font-semibold text-white">
          &#9733; Default
        </span>
      )}
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
      <div className="mt-2 flex flex-wrap gap-1.5">
        <Button
          variant="outline"
          size="sm"
          className="border-red-300 text-red-600 hover:bg-red-50 hover:text-red-700 dark:border-red-900 dark:hover:bg-red-950/30"
          onClick={() => onRemove(face.face_id)}
        >
          Remove
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={face.is_primary}
          title={
            face.is_primary
              ? "Already the default photo"
              : "Show this photo for this person on the labeling page"
          }
          onClick={() => onSetDefault(face.face_id)}
        >
          Set Default
        </Button>
      </div>
    </div>
  );
}

export function PersonPage() {
  const params = useParams<{ name: string }>();
  const personName = decodeURIComponent(params.name ?? "");
  const navigate = useNavigate();
  const client = useClient();

  const { data, isLoading, error } = usePersonDetail(personName);
  const { removeFace, setPrimary, renamePerson, deletePerson } = useMutations();

  const [renameValue, setRenameValue] = useState(personName);
  const [renameError, setRenameError] = useState<string | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [lightboxFaceId, setLightboxFaceId] = useState<number | null>(null);

  const doRename = async () => {
    const newLabel = sanitizeName(renameValue);
    if (!newLabel || newLabel === personName) return;
    setRenameError(null);
    try {
      await renamePerson.mutateAsync({ oldLabel: personName, newLabel });
      navigate("/person/" + encodeURIComponent(newLabel));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.includes("conflict")) {
        setRenameError(`A person named "${newLabel}" already exists`);
      } else {
        setRenameError("Rename failed.");
      }
    }
  };

  const doRemovePerson = () => {
    deletePerson.mutate(personName, { onSuccess: () => navigate("/") });
    setConfirmOpen(false);
  };

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
        <a
          href="/"
          onClick={(e) => {
            e.preventDefault();
            navigate("/");
          }}
          className="text-sm text-blue-600 hover:underline dark:text-blue-400"
        >
          &larr; Back to labeling
        </a>
        <strong>{data.label}</strong>
        <span className="text-sm text-muted-foreground">{data.faces.length} face(s)</span>

        <span className="flex items-center gap-1.5">
          <Input
            value={renameValue}
            maxLength={MAX_NAME_LEN}
            onChange={(e) => {
              setRenameValue(e.target.value);
              setRenameError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                doRename();
              }
            }}
            className="h-8 w-40"
          />
          <Button size="sm" onClick={doRename}>
            Save
          </Button>
        </span>
        {renameError && <span className="text-sm text-destructive">{renameError}</span>}

        <Button
          variant="outline"
          size="sm"
          className="ml-auto border-red-300 text-red-600 hover:bg-red-50 hover:text-red-700 dark:border-red-900 dark:hover:bg-red-950/30"
          onClick={() => setConfirmOpen(true)}
        >
          Remove person
        </Button>
      </div>

      <div className="grid grid-cols-[repeat(auto-fill,200px)] gap-3.5">
        {data.faces.map((f) => (
          <FaceCard
            key={f.face_id}
            face={f}
            onRemove={(faceId) => removeFace.mutate(faceId)}
            onSetDefault={(faceId) => setPrimary.mutate({ faceId, label: personName })}
            onOpen={setLightboxFaceId}
          />
        ))}
      </div>

      <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Remove {personName}?</DialogTitle>
            <DialogDescription>
              Their {data.faces.length} photo(s) will become unassigned.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmOpen(false)}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={doRemovePerson}>
              Remove person
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

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
