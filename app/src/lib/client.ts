import { invoke } from "@tauri-apps/api/core";
import type { FacesData, ClusterDetail, PersonDetail } from "./models";

export interface VidereClient {
  facesList(): Promise<FacesData>;
  clusterDetail(clusterId: number): Promise<ClusterDetail>;
  personDetail(name: string): Promise<PersonDetail>;
  searchPerson(name: string): Promise<string[]>;
  assign(faceIds: number[], personLabel: string): Promise<void>;
  newPerson(faceIds: number[], label: string): Promise<void>;
  removeFace(faceId: number): Promise<void>;
  dissolveCluster(clusterId: number): Promise<void>;
  deletePerson(label: string): Promise<void>;
  setPrimary(faceId: number, personLabel: string): Promise<void>;
  renamePerson(oldLabel: string, newLabel: string): Promise<void>;
  faceImageUrl(faceId: number): string;
  originalImageUrl(faceId: number): string;
}

export class TauriClient implements VidereClient {
  facesList() { return invoke<FacesData>("faces_list"); }
  clusterDetail(clusterId: number) { return invoke<ClusterDetail>("cluster_detail", { cluster_id: clusterId }); }
  personDetail(name: string) { return invoke<PersonDetail>("person_detail", { name }); }
  searchPerson(name: string) { return invoke<string[]>("search_person", { name }); }
  assign(faceIds: number[], personLabel: string) { return invoke<void>("assign", { face_ids: faceIds, person_label: personLabel }); }
  newPerson(faceIds: number[], label: string) { return invoke<void>("new_person", { face_ids: faceIds, label }); }
  removeFace(faceId: number) { return invoke<void>("remove_face", { face_id: faceId }); }
  dissolveCluster(clusterId: number) { return invoke<void>("dissolve_cluster", { cluster_id: clusterId }); }
  deletePerson(label: string) { return invoke<void>("delete_person", { label }); }
  setPrimary(faceId: number, personLabel: string) { return invoke<void>("set_primary", { face_id: faceId, person_label: personLabel }); }
  renamePerson(oldLabel: string, newLabel: string) { return invoke<void>("rename_person", { old_label: oldLabel, new_label: newLabel }); }
  faceImageUrl(faceId: number) { return `videre-face://${faceId}`; }
  originalImageUrl(faceId: number) { return `videre-original://${faceId}`; }
}
