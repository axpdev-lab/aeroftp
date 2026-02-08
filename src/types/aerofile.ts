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

/** Subdirectory entry returned by Rust `list_subdirectories` command */
export interface SubDirectory {
  name: string;
  path: string;
  has_children: boolean;
}

/** Sidebar display mode */
export type SidebarMode = 'places' | 'tree';
