export interface PersonData { label: string; face_ids: number[]; representative_id: number; hashes: string[]; }
export interface ClusterData { cluster_id: number; face_ids: number[]; hashes: string[]; }
export interface SingletonData { face_id: number; hash: string; }
export interface FacesData { people: PersonData[]; clusters: ClusterData[]; singletons: SingletonData[]; }
export interface ClusterFaceData { face_id: number; hash: string; path: string; }
export interface ClusterDetail { cluster_id: number; faces: ClusterFaceData[]; }
export interface PersonFaceData { face_id: number; hash: string; path: string; is_primary: boolean; }
export interface PersonDetail { label: string; faces: PersonFaceData[]; }
