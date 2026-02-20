// AeroFile mode shared types
// Used by PlacesSidebar, FolderTree, and future AeroFile components

/** User directory returned by Rust `get_user_directories` command */
export interface UserDirectory {
  key: string;       // "home", "desktop", "documents", etc.
  path: string;      // absolute path
  icon: string;      // Lucide icon name (e.g. "Home", "Monitor")
}

/** Volume/mount info returned by Rust `list_mounted_volumes` command */
export interface VolumeInfo {
  name: string;
  mount_point: string;
  volume_type: 'internal' | 'removable' | 'network' | 'optical';
  total_bytes: number;
  free_bytes: number;
  fs_type: string;
  is_ejectable: boolean;
}

/** Unmounted partition detected by Rust `list_unmounted_partitions` command */
export interface UnmountedPartition {
  name: string;
  device: string;
  fs_type: string;
  size_bytes: number;
}

/** Subdirectory entry returned by Rust `list_subdirectories` command */
export interface SubDirectory {
  name: string;
  path: string;
  has_children: boolean;
}

/** Sidebar display mode */
export type SidebarMode = 'places' | 'tree';

/** Item in the system trash / recycle bin */
export interface TrashItem {
  id: string;
  name: string;
  original_path: string;
  deleted_at: string | null;
  size: number;
  is_dir: boolean;
}

/** Aggregated size result for a folder */
export interface FolderSizeResult {
  total_bytes: number;
  file_count: number;
  dir_count: number;
}

/** Group of duplicate files sharing the same content hash */
export interface DuplicateGroup {
  hash: string;
  size: number;
  files: string[];
}

/** Node in a disk usage tree for treemap visualization */
export interface DiskUsageNode {
  name: string;
  path: string;
  size: number;
  is_dir: boolean;
  children?: DiskUsageNode[] | null;
}

/** Local path tab for multi-tab file browsing in AeroFile mode */
export interface LocalTab {
  id: string;
  path: string;
  label: string;           // last path segment (folder name)
  scrollTop: number;
}

/** Tag label (preset or custom color label) */
export interface TagLabel {
  id: number;
  name: string;
  color: string;
  sort_order: number;
  is_preset: boolean;
}

/** File-tag association (joined with label info) */
export interface FileTag {
  id: number;
  file_path: string;
  label_id: number;
  label_name: string;
  label_color: string;
  created_at: number;
}

/** Label with file count for sidebar display */
export interface LabelCount {
  id: number;
  name: string;
  color: string;
  count: number;
}

/** Detailed file properties returned by Rust `get_file_properties` command */
export interface DetailedFileProperties {
  name: string;
  path: string;
  size: number;
  is_dir: boolean;
  created: string | null;
  modified: string | null;
  accessed: string | null;
  permissions_mode: number | null;
  permissions_text: string | null;
  owner: string | null;
  group: string | null;
  is_symlink: boolean;
  link_target: string | null;
  inode: number | null;
  hard_links: number | null;
}
