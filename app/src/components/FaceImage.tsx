import { useClient } from "@/lib/ClientProvider";
export function FaceImage({ faceId, size = 140, className }: { faceId: number; size?: number; className?: string }) {
  const c = useClient();
  return (
    <img src={c.faceImageUrl(faceId)} width={size} height={size} loading="lazy" alt={`face ${faceId}`}
      className={className}
      style={{ objectFit: "cover", aspectRatio: "1 / 1", maxWidth: "100%", height: "auto", background: "#e5e7eb", borderRadius: 6 }}
      onError={(e) => { (e.currentTarget as HTMLImageElement).style.visibility = "hidden"; }} />
  );
}
