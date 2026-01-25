// i18n Type Definitions
// Provides full TypeScript support for translations

/**
 * Supported language codes (ISO 639-1)
 */
export type Language = 'en' | 'it' | 'es' | 'fr' | 'zh';

/**
 * Translation namespace structure
 * Organized by feature/component area for maintainability
 */
export interface TranslationKeys {
    // Common UI elements
    common: {
        save: string;
        cancel: string;
        close: string;
        delete: string;
        edit: string;
        create: string;
        refresh: string;
        search: string;
        loading: string;
        error: string;
        success: string;
        warning: string;
        info: string;
        confirm: string;
        yes: string;
        no: string;
        ok: string;
        back: string;
        next: string;
        finish: string;
        browse: string;
        select: string;
        copy: string;
        paste: string;
        cut: string;
        rename: string;
        download: string;
        upload: string;
        connect: string;
        disconnect: string;
        settings: string;
        help: string;
        about: string;
        version: string;
        language: string;
        theme: string;
        light: string;
        dark: string;
        auto: string;
    };

    // Connection screen
    connection: {
        title: string;
        server: string;
        serverPlaceholder: string;
        username: string;
        usernamePlaceholder: string;
        password: string;
        passwordPlaceholder: string;
        port: string;
        protocol: string;
        ftp: string;
        ftps: string;
        sftp: string;
        rememberPassword: string;
        connecting: string;
        connected: string;
        disconnected: string;
        connectionFailed: string;
        reconnecting: string;
        quickConnect: string;
        savedServers: string;
        noSavedServers: string;
        saveServer: string;
        deleteServer: string;
        editServer: string;
        serverName: string;
        initialPath: string;
    };

    // File browser
    browser: {
        remote: string;
        local: string;
        name: string;
        size: string;
        modified: string;
        type: string;
        permissions: string;
        path: string;
        files: string;
        folders: string;
        items: string;
        emptyFolder: string;
        parentFolder: string;
        newFolder: string;
        newFolderName: string;
        deleteConfirm: string;
        deleteConfirmMultiple: string;
        renameTitle: string;
        renamePlaceholder: string;
        uploadFiles: string;
        uploadFolder: string;
        downloadFiles: string;
        refreshList: string;
        showHiddenFiles: string;
        listView: string;
        gridView: string;
        sortBy: string;
        ascending: string;
        descending: string;
        selected: string;
        selectAll: string;
        deselectAll: string;
    };

    // Context menu
    contextMenu: {
        open: string;
        preview: string;
        edit: string;
        viewSource: string;
        copyPath: string;
        openInTerminal: string;
        openInFileManager: string;
        properties: string;
        permissions: string;
        compress: string;
        extract: string;
    };

    // Transfer
    transfer: {
        transferring: string;
        paused: string;
        completed: string;
        failed: string;
        cancelled: string;
        progress: string;
        speed: string;
        remaining: string;
        elapsed: string;
        queue: string;
        clearCompleted: string;
        cancelAll: string;
        pauseAll: string;
        resumeAll: string;
    };

    // Settings panel
    settings: {
        title: string;
        general: string;
        connection: string;
        transfers: string;
        appearance: string;
        advanced: string;
        defaultLocalPath: string;
        confirmBeforeDelete: string;
        showStatusBar: string;
        compactMode: string;
        timeout: string;
        seconds: string;
        minutes: string;
        maxConcurrentTransfers: string;
        retryCount: string;
        preserveTimestamps: string;
        transferMode: string;
        ascii: string;
        binary: string;
        selectLanguage: string;
        interfaceLanguage: string;
        restartRequired: string;
    };

    // DevTools panel
    devtools: {
        title: string;
        preview: string;
        editor: string;
        terminal: string;
        agent: string;
        saveChanges: string;
        discardChanges: string;
        filePreview: string;
        noFileSelected: string;
        unsavedChanges: string;
        syntaxHighlighting: string;
        wordWrap: string;
        lineNumbers: string;
        minimap: string;
    };

    // AeroCloud
    cloud: {
        title: string;
        setup: string;
        dashboard: string;
        syncNow: string;
        pause: string;
        resume: string;
        disable: string;
        enable: string;
        openFolder: string;
        localFolder: string;
        remoteFolder: string;
        serverProfile: string;
        syncInterval: string;
        lastSync: string;
        nextSync: string;
        syncing: string;
        synced: string;
        pending: string;
        conflict: string;
        error: string;
        never: string;
        justNow: string;
        minutesAgo: string;
        hoursAgo: string;
        cloudName: string;
        cloudNamePlaceholder: string;
        cloudNameDesc: string;
        selectServer: string;
        syncOnChange: string;
        stepFolder: string;
        stepServer: string;
        stepSettings: string;
        enableCloud: string;
        disableCloud: string;
    };

    // Status bar
    statusBar: {
        connected: string;
        notConnected: string;
        syncing: string;
        syncFiles: string;
        devTools: string;
    };

    // Dialogs
    dialogs: {
        confirmTitle: string;
        inputTitle: string;
        errorTitle: string;
        successTitle: string;
        warningTitle: string;
    };

    // Toast messages
    toast: {
        connectionSuccess: string;
        connectionFailed: string;
        disconnected: string;
        uploadStarted: string;
        uploadComplete: string;
        uploadFailed: string;
        downloadStarted: string;
        downloadComplete: string;
        downloadFailed: string;
        deleteSuccess: string;
        deleteFailed: string;
        renameSuccess: string;
        renameFailed: string;
        folderCreated: string;
        folderCreateFailed: string;
        settingsSaved: string;
        clipboardCopied: string;
        syncStarted: string;
        syncComplete: string;
        syncFailed: string;
    };

    // About dialog
    about: {
        tagline: string;
        features: {
            rustEngine: string;
            monacoEditor: string;
            ptyTerminal: string;
            aiAgent: string;
            ftpsSecure: string;
            fileSync: string;
            aeroCloud: string;
            mediaPlayer: string;
            imagePreview: string;
        };
        madeWith: string;
        aiCredits: string;
        copyright: string;
        supportDev: string;
        donateWith: string;
    };

    // Support dialog
    support: {
        title: string;
        subtitle: string;
        fiatSection: string;
        cryptoSection: string;
        thanks: string;
        footer: string;
    };
}

/**
 * Full translation object with metadata
 */
export interface Translation {
    meta: {
        code: Language;
        name: string;
        nativeName: string;
        direction: 'ltr' | 'rtl';
    };
    translations: TranslationKeys;
}

/**
 * i18n Context value
 */
export interface I18nContextValue {
    language: Language;
    setLanguage: (lang: Language) => void;
    t: TranslationFunction;
    availableLanguages: LanguageInfo[];
}

/**
 * Language metadata for UI display
 */
export interface LanguageInfo {
    code: Language;
    name: string;
    nativeName: string;
    flag: string; // Emoji flag
}

/**
 * Translation function type
 * Supports dot notation: t('common.save')
 */
export type TranslationFunction = (
    key: string,
    params?: Record<string, string | number>
) => string;

/**
 * Available languages with metadata
 */
export const AVAILABLE_LANGUAGES: LanguageInfo[] = [
    { code: 'en', name: 'English', nativeName: 'English', flag: '🇬🇧' },
    { code: 'it', name: 'Italian', nativeName: 'Italiano', flag: '🇮🇹' },
    { code: 'es', name: 'Spanish', nativeName: 'Español', flag: '🇪🇸' },
    { code: 'fr', name: 'French', nativeName: 'Français', flag: '🇫🇷' },
    { code: 'zh', name: 'Chinese', nativeName: '简体中文', flag: '🇨🇳' },
];

/**
 * Default/fallback language
 */
export const DEFAULT_LANGUAGE: Language = 'en';

/**
 * LocalStorage key for persisting language preference
 */
export const LANGUAGE_STORAGE_KEY = 'aeroftp_language';
