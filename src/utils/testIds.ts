// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Stable GUI addresses. Repeated elements are qualified by entity attributes.
 * These ids grant no permission; sensitive controls carry data-agent="deny".
 */
export const TID = {
    serverCard: 'servers.card',
    serverCardConnect: 'servers.card.connect',
    serverRow: 'servers.row',
    serverRowConnect: 'servers.row.connect',
    titlebarDisconnect: 'titlebar.disconnect',
    titlebarLock: 'titlebar.lock',
    sessionTab: 'sessions.tab',
    sessionTabClose: 'sessions.tab.close',
    panel: 'panel',
    panelRefresh: 'panel.refresh',
    fileRow: 'panel.file',
    breadcrumbPath: 'breadcrumb.path',
    breadcrumbInput: 'breadcrumb.input',
    breadcrumbEdit: 'breadcrumb.edit',
    breadcrumbConfirm: 'breadcrumb.confirm',
    breadcrumbUp: 'breadcrumb.up',
    toolbarTransfer: 'toolbar.transfer',
    toolbarRefresh: 'toolbar.refresh',
    toolbarMkdir: 'toolbar.mkdir',
    toolbarDelete: 'toolbar.delete',
    toolbarCancelAll: 'toolbar.cancel-all',
    queueItem: 'queue.item',
    queueStopAll: 'queue.stop-all',
    confirmDialog: 'dialog.confirm',
    confirmOk: 'dialog.confirm.ok',
    confirmCancel: 'dialog.confirm.cancel',
    inputDialog: 'dialog.input',
    inputField: 'dialog.input.field',
    inputOk: 'dialog.input.ok',
    inputCancel: 'dialog.input.cancel',
    overwriteDialog: 'dialog.overwrite',
    overwriteSkip: 'dialog.overwrite.skip',
    overwriteConfirm: 'dialog.overwrite.overwrite',
    overwriteRename: 'dialog.overwrite.rename',
    menuItem: 'menu.item',
    settingsTab: 'settings.tab',
} as const;

export type GuiPanelId = 'remote' | 'local' | 'local2';
