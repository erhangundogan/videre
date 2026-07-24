import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useClient } from "./ClientProvider";

export function useFacesList() {
  const c = useClient();
  return useQuery({ queryKey: ["faces"], queryFn: () => c.facesList() });
}
export function useClusterDetail(id: number) {
  const c = useClient();
  return useQuery({ queryKey: ["cluster", id], queryFn: () => c.clusterDetail(id) });
}
export function usePersonDetail(name: string) {
  const c = useClient();
  return useQuery({ queryKey: ["person", name], queryFn: () => c.personDetail(name) });
}
export function useMutations() {
  const c = useClient();
  const qc = useQueryClient();
  const inval = () => qc.invalidateQueries();
  return {
    assign: useMutation({ mutationFn: (v: { faceIds: number[]; label: string }) => c.assign(v.faceIds, v.label), onSuccess: inval }),
    newPerson: useMutation({ mutationFn: (v: { faceIds: number[]; label: string }) => c.newPerson(v.faceIds, v.label), onSuccess: inval }),
    removeFace: useMutation({ mutationFn: (id: number) => c.removeFace(id), onSuccess: inval }),
    dissolveCluster: useMutation({ mutationFn: (id: number) => c.dissolveCluster(id), onSuccess: inval }),
    deletePerson: useMutation({ mutationFn: (label: string) => c.deletePerson(label), onSuccess: inval }),
    setPrimary: useMutation({ mutationFn: (v: { faceId: number; label: string }) => c.setPrimary(v.faceId, v.label), onSuccess: inval }),
    renamePerson: useMutation({ mutationFn: (v: { oldLabel: string; newLabel: string }) => c.renamePerson(v.oldLabel, v.newLabel), onSuccess: inval }),
  };
}
