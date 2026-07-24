import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useFacesList, useMutations } from "@/lib/queries";
import { FaceImage } from "@/components/FaceImage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import type { PersonData } from "@/lib/models";
import { cn } from "@/lib/utils";

const MAX_NAME_LEN = 60;
const LAYOUT_KEY = "videre_people_layout";
const SINGLETON_INITIAL_COUNT = 200;
const SINGLETON_LOAD_MORE_OPTIONS = [100, 200, 500, 1000];

// Trim, collapse internal whitespace, strip control/bidi-spoofing characters,
// and cap length by code point (not UTF-16 code unit) so a pasted wall of
// text or a spoofed name can't stretch card layout, corrupt display order,
// or bloat the DB. Mirrors the sanitizeName() in FACES_HTML; the backend
// also sanitizes.
function sanitizeName(raw: string): string {
  const filtered = Array.from(raw)
    .filter((ch) => {
      const cp = ch.codePointAt(0)!;
      if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
      if (cp === 0x200b) return false;
      if (cp === 0x200e || cp === 0x200f) return false;
      // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed - required
      // for Persian/Indic text and emoji ZWJ sequences.
      if (cp >= 0x202a && cp <= 0x202e) return false;
      if (cp >= 0x2060 && cp <= 0x2069) return false;
      if (cp === 0xfeff) return false;
      return true;
    })
    .join("");
  const collapsed = filtered.trim().replace(/\s+/g, " ");
  return Array.from(collapsed).slice(0, MAX_NAME_LEN).join("");
}

function dragFaceIds(e: React.DragEvent): number[] | null {
  const raw = e.dataTransfer.getData("application/json");
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed.face_ids)) return parsed.face_ids;
  } catch {
    // ignore malformed payload
  }
  return null;
}

function ThumbGrid({ faceIds }: { faceIds: number[] }) {
  if (faceIds.length === 1) {
    return (
      <div className="mb-1.5">
        <FaceImage faceId={faceIds[0]} size={140} className="w-full" />
      </div>
    );
  }
  const visible = faceIds.slice(0, 4);
  const extra = faceIds.length - 4;
  return (
    <div className="mb-1.5">
      <div className="grid grid-cols-2 gap-1">
        {visible.map((id) => (
          <FaceImage key={id} faceId={id} size={66} />
        ))}
      </div>
      {extra > 0 && (
        <div className="mt-0.5 text-[11px] font-medium text-muted-foreground">
          +{extra} more
        </div>
      )}
    </div>
  );
}

function NewPersonInline({
  faceIds,
  colorClasses,
  onCreate,
}: {
  faceIds: number[];
  colorClasses: { border: string; text: string; solidBg: string };
  onCreate: (faceIds: number[], label: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");

  if (!editing) {
    return (
      <Button
        variant="outline"
        size="sm"
        className={cn("mt-2 w-full", colorClasses.border, colorClasses.text)}
        onClick={() => setEditing(true)}
      >
        New Person
      </Button>
    );
  }

  const submit = () => {
    const label = sanitizeName(value);
    if (!label) return;
    onCreate(faceIds, label);
    setEditing(false);
    setValue("");
  };

  return (
    <div className="mt-2 flex flex-col gap-1.5">
      <Input
        autoFocus
        placeholder="Person name"
        maxLength={MAX_NAME_LEN}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
      />
      <div className="flex gap-1">
        <Button size="sm" className={cn("flex-1", colorClasses.solidBg)} onClick={submit}>
          Create
        </Button>
        <Button
          variant="outline"
          size="sm"
          className="flex-1"
          onClick={() => {
            setEditing(false);
            setValue("");
          }}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}

function PersonCard({
  person,
  onDropAssign,
}: {
  person: PersonData;
  onDropAssign: (faceIds: number[], label: string) => void;
}) {
  const [dragOver, setDragOver] = useState(false);
  const url = `/person/${encodeURIComponent(person.label)}`;
  const extra = person.face_ids.length - 1;

  return (
    <div
      className={cn(
        "w-40 rounded-lg border-2 bg-blue-50 p-2.5 dark:bg-blue-950/30",
        dragOver ? "border-blue-600 bg-blue-100 dark:bg-blue-900/40" : "border-blue-400/70"
      )}
      onDragOver={(e) => {
        e.preventDefault();
        setDragOver(true);
      }}
      onDragLeave={() => setDragOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragOver(false);
        const ids = dragFaceIds(e);
        if (ids && ids.length > 0) onDropAssign(ids, person.label);
      }}
    >
      <Link to={url}>
        <div className="mb-1.5">
          <FaceImage faceId={person.representative_id} size={140} className="w-full" />
        </div>
      </Link>
      <Link
        to={url}
        title={person.label}
        className="block truncate font-bold text-blue-700 hover:underline dark:text-blue-300"
      >
        {person.label}
      </Link>
      {extra > 0 && (
        <div className="mt-0.5 text-[11px] font-medium text-blue-700 dark:text-blue-300">
          +{extra} more
        </div>
      )}
    </div>
  );
}

function AssignableCard({
  faceIds,
  linkUrl,
  variant,
  selectable,
  selected,
  onToggleSelect,
  onDragStart,
  onCreatePerson,
}: {
  faceIds: number[];
  linkUrl?: string;
  variant: "cluster" | "singleton";
  selectable?: boolean;
  selected?: boolean;
  onToggleSelect?: () => void;
  onDragStart: (e: React.DragEvent) => void;
  onCreatePerson: (faceIds: number[], label: string) => void;
}) {
  const colors =
    variant === "cluster"
      ? {
          border: "border-green-500/70",
          bg: "bg-green-50 dark:bg-green-950/30",
          text: "text-green-700 dark:text-green-300",
          solidBg: "bg-green-600 hover:bg-green-700 text-white",
        }
      : {
          border: "border-orange-400/70",
          bg: "bg-orange-50 dark:bg-orange-950/30",
          text: "text-orange-700 dark:text-orange-300",
          solidBg: "bg-orange-500 hover:bg-orange-600 text-white",
        };

  const inner = <ThumbGrid faceIds={faceIds} />;

  let thumb: React.ReactNode;
  if (selectable) {
    thumb = (
      <div
        className="relative cursor-pointer"
        title="Click to select"
        onClick={onToggleSelect}
      >
        {selected && (
          <div className="absolute right-1.5 top-1.5 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-blue-600 text-xs text-white">
            &#10003;
          </div>
        )}
        {inner}
      </div>
    );
  } else if (linkUrl) {
    thumb = <Link to={linkUrl}>{inner}</Link>;
  } else {
    thumb = inner;
  }

  return (
    <div
      className={cn(
        "w-40 rounded-lg border-2 p-2.5",
        colors.border,
        colors.bg,
        selected && "border-blue-600 ring-2 ring-blue-600"
      )}
    >
      <div
        draggable
        onDragStart={onDragStart}
        title="Drag to assign to a person"
        className={cn(
          "mb-1.5 flex cursor-grab select-none items-center gap-1.5 text-[11px]",
          colors.text,
          "opacity-70 hover:opacity-100"
        )}
      >
        <span className="text-base leading-none">&#8942;&#8942;&#8942;</span>
        <span className="text-[10px] leading-tight">Drag on person above</span>
      </div>
      {thumb}
      <NewPersonInline faceIds={faceIds} colorClasses={colors} onCreate={onCreatePerson} />
    </div>
  );
}

export function LabelingPage() {
  const { data, isLoading, error } = useFacesList();
  const { assign, newPerson } = useMutations();

  const [layout, setLayout] = useState<"top" | "right">(
    () => (localStorage.getItem(LAYOUT_KEY) as "top" | "right") || "top"
  );
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [creatingFromSelection, setCreatingFromSelection] = useState(false);
  const [selectionLabel, setSelectionLabel] = useState("");
  const [visibleSingletonCount, setVisibleSingletonCount] = useState(SINGLETON_INITIAL_COUNT);

  const toggleLayout = () => {
    const next = layout === "right" ? "top" : "right";
    localStorage.setItem(LAYOUT_KEY, next);
    setLayout(next);
  };

  const sortedPeople = useMemo(() => {
    if (!data) return [];
    return [...data.people].sort((a, b) =>
      a.label.localeCompare(b.label, undefined, { sensitivity: "base" })
    );
  }, [data]);

  const sortedClusters = useMemo(() => {
    if (!data) return [];
    return [...data.clusters].sort((a, b) => b.face_ids.length - a.face_ids.length);
  }, [data]);

  const doAssign = (faceIds: number[], label: string) => {
    assign.mutate({ faceIds, label });
    setSelected(new Set());
  };

  const doNewPerson = (faceIds: number[], label: string) => {
    newPerson.mutate({ faceIds, label });
    setSelected(new Set());
  };

  const toggleSingleton = (faceId: number) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(faceId)) next.delete(faceId);
      else next.add(faceId);
      return next;
    });
  };

  const dragStartFor = (faceIds: number[], selId?: number) => (e: React.DragEvent) => {
    let ids = faceIds;
    if (selId != null && selected.has(selId) && selected.size > 1) {
      ids = Array.from(selected);
    }
    e.dataTransfer.setData("application/json", JSON.stringify({ face_ids: ids }));
  };

  const submitSelectionPerson = () => {
    const label = sanitizeName(selectionLabel);
    if (!label) return;
    doNewPerson(Array.from(selected), label);
    setCreatingFromSelection(false);
    setSelectionLabel("");
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

  const peopleSection = (
    <div className={cn(layout === "right" ? "" : "sticky top-0 z-10 bg-background pb-2")}>
      <h2 className="mb-2 flex items-center gap-2 border-b pb-1 text-lg font-semibold text-blue-700 dark:text-blue-300">
        People
        <Badge className="border-blue-400 bg-blue-50 text-blue-700 dark:bg-blue-950/30 dark:text-blue-300">
          {sortedPeople.length}
        </Badge>
      </h2>
      <div
        className={cn(
          layout === "right" ? "max-h-none overflow-visible" : "max-h-[45vh] overflow-y-auto pb-1"
        )}
      >
        <div
          className={cn(
            "grid gap-3",
            layout === "right"
              ? "grid-cols-[repeat(auto-fill,132px)]"
              : "grid-cols-[repeat(auto-fill,160px)]"
          )}
        >
          {sortedPeople.map((p) => (
            <PersonCard key={p.label} person={p} onDropAssign={doAssign} />
          ))}
        </div>
      </div>
    </div>
  );

  return (
    <div className={cn("p-4", layout === "right" && "pr-[316px]")}>
      <div className="mb-4 flex items-center gap-3">
        <strong>videre faces labeling</strong>
        <span className="text-sm text-muted-foreground">
          {data.people.length} people, {data.clusters.length} clusters, {data.singletons.length}{" "}
          singletons
        </span>
        <Button variant="outline" size="sm" onClick={toggleLayout}>
          People: {layout === "right" ? "Right" : "Top"}
        </Button>
      </div>

      {layout === "right" ? (
        <div className="fixed bottom-0 right-0 top-0 z-20 w-[300px] overflow-y-auto border-l bg-background p-4 shadow-lg">
          {peopleSection}
        </div>
      ) : (
        peopleSection
      )}

      <h2 className="mb-2 mt-4 flex items-center gap-2 border-b pb-1 text-lg font-semibold text-green-700 dark:text-green-300">
        Unassigned Clusters
        <Badge className="border-green-500 bg-green-50 text-green-700 dark:bg-green-950/30 dark:text-green-300">
          {sortedClusters.length}
        </Badge>
      </h2>
      <div className="mb-6 grid grid-cols-[repeat(auto-fill,160px)] gap-3">
        {sortedClusters.map((c) => (
          <AssignableCard
            key={c.cluster_id}
            faceIds={c.face_ids}
            linkUrl={`/cluster/${c.cluster_id}`}
            variant="cluster"
            onDragStart={dragStartFor(c.face_ids)}
            onCreatePerson={doNewPerson}
          />
        ))}
      </div>

      <h2 className="mb-2 flex items-center gap-2 border-b pb-1 text-lg font-semibold text-orange-700 dark:text-orange-300">
        Singletons
        <Badge className="border-orange-400 bg-orange-50 text-orange-700 dark:bg-orange-950/30 dark:text-orange-300">
          {data.singletons.length}
        </Badge>
      </h2>
      <div className="mb-3 grid grid-cols-[repeat(auto-fill,160px)] gap-3">
        {data.singletons.slice(0, visibleSingletonCount).map((s) => (
          <AssignableCard
            key={s.face_id}
            faceIds={[s.face_id]}
            variant="singleton"
            selectable
            selected={selected.has(s.face_id)}
            onToggleSelect={() => toggleSingleton(s.face_id)}
            onDragStart={dragStartFor([s.face_id], s.face_id)}
            onCreatePerson={doNewPerson}
          />
        ))}
      </div>

      {visibleSingletonCount < data.singletons.length && (
        <div className="mb-6 flex flex-wrap items-center gap-2">
          <span className="text-sm text-muted-foreground">
            Showing {visibleSingletonCount} of {data.singletons.length}
          </span>
          {SINGLETON_LOAD_MORE_OPTIONS.map((n) => (
            <Button
              key={n}
              variant="outline"
              size="sm"
              onClick={() => setVisibleSingletonCount((v) => v + n)}
            >
              +{n} more
            </Button>
          ))}
          <Button
            variant="outline"
            size="sm"
            onClick={() => setVisibleSingletonCount(data.singletons.length)}
          >
            Show all
          </Button>
        </div>
      )}

      {selected.size > 0 && (
        <div className="fixed bottom-5 left-1/2 z-30 flex -translate-x-1/2 flex-wrap items-center gap-3 rounded-xl bg-blue-600 px-4 py-2.5 text-white shadow-xl">
          {creatingFromSelection ? (
            <>
              <Input
                autoFocus
                placeholder="Person name"
                maxLength={MAX_NAME_LEN}
                value={selectionLabel}
                onChange={(e) => setSelectionLabel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    submitSelectionPerson();
                  }
                }}
                className="w-40 bg-white text-black"
              />
              <Button size="sm" variant="secondary" onClick={submitSelectionPerson}>
                Create
              </Button>
              <Button size="sm" variant="secondary" onClick={() => setCreatingFromSelection(false)}>
                Cancel
              </Button>
            </>
          ) : (
            <>
              <span className="font-semibold">{selected.size} selected</span>
              <Button size="sm" variant="secondary" onClick={() => setCreatingFromSelection(true)}>
                New Person
              </Button>
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  setSelected(new Set());
                  setCreatingFromSelection(false);
                }}
              >
                Clear
              </Button>
              <span className="text-xs opacity-85">or drag any selected onto a person</span>
            </>
          )}
        </div>
      )}
    </div>
  );
}
