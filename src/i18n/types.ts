// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// i18n Type Definitions
// Provides full TypeScript support for translations

/**
 * Supported language codes (ISO 639-1)
 * 47 languages - More than Cyberduck (31)!
 */
export type Language =
    // Existing (5)
    | 'en' | 'it' | 'es' | 'fr' | 'zh'
    // Major European (12)
    | 'de' | 'pt' | 'ru' | 'nl' | 'pl' | 'uk' | 'ro' | 'cs' | 'hu' | 'el' | 'bg' | 'sk'
    // Nordic (5)
    | 'sv' | 'da' | 'no' | 'fi' | 'is'
    // Asian (10)
    | 'ja' | 'ko' | 'vi' | 'th' | 'id' | 'ms' | 'tl' | 'km' | 'hi' | 'bn'
    // Balkan & Caucasus (6)
    | 'hr' | 'sr' | 'sl' | 'mk' | 'ka' | 'hy'
    // Baltic (3)
    | 'lt' | 'lv' | 'et'
    // Celtic & Iberian (4)
    | 'cy' | 'gl' | 'ca' | 'eu'
    // African (1)
    | 'sw'
    // Turkish (1)
    | 'tr';

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
    // Existing (5)
    { code: 'en', name: 'English', nativeName: 'English', flag: '🇬🇧' },
    { code: 'it', name: 'Italian', nativeName: 'Italiano', flag: '🇮🇹' },
    { code: 'es', name: 'Spanish', nativeName: 'Español', flag: '🇪🇸' },
    { code: 'fr', name: 'French', nativeName: 'Français', flag: '🇫🇷' },
    { code: 'zh', name: 'Chinese', nativeName: '简体中文', flag: '🇨🇳' },
    // Major European (12)
    { code: 'de', name: 'German', nativeName: 'Deutsch', flag: '🇩🇪' },
    { code: 'pt', name: 'Portuguese', nativeName: 'Português', flag: '🇵🇹' },
    { code: 'ru', name: 'Russian', nativeName: 'Русский', flag: '🇷🇺' },
    { code: 'nl', name: 'Dutch', nativeName: 'Nederlands', flag: '🇳🇱' },
    { code: 'pl', name: 'Polish', nativeName: 'Polski', flag: '🇵🇱' },
    { code: 'uk', name: 'Ukrainian', nativeName: 'Українська', flag: '🇺🇦' },
    { code: 'ro', name: 'Romanian', nativeName: 'Română', flag: '🇷🇴' },
    { code: 'cs', name: 'Czech', nativeName: 'Čeština', flag: '🇨🇿' },
    { code: 'hu', name: 'Hungarian', nativeName: 'Magyar', flag: '🇭🇺' },
    { code: 'el', name: 'Greek', nativeName: 'Ελληνικά', flag: '🇬🇷' },
    { code: 'bg', name: 'Bulgarian', nativeName: 'Български', flag: '🇧🇬' },
    { code: 'sk', name: 'Slovak', nativeName: 'Slovenčina', flag: '🇸🇰' },
    // Nordic (5)
    { code: 'sv', name: 'Swedish', nativeName: 'Svenska', flag: '🇸🇪' },
    { code: 'da', name: 'Danish', nativeName: 'Dansk', flag: '🇩🇰' },
    { code: 'no', name: 'Norwegian', nativeName: 'Norsk', flag: '🇳🇴' },
    { code: 'fi', name: 'Finnish', nativeName: 'Suomi', flag: '🇫🇮' },
    { code: 'is', name: 'Icelandic', nativeName: 'Íslenska', flag: '🇮🇸' },
    // Asian (10)
    { code: 'ja', name: 'Japanese', nativeName: '日本語', flag: '🇯🇵' },
    { code: 'ko', name: 'Korean', nativeName: '한국어', flag: '🇰🇷' },
    { code: 'vi', name: 'Vietnamese', nativeName: 'Tiếng Việt', flag: '🇻🇳' },
    { code: 'th', name: 'Thai', nativeName: 'ไทย', flag: '🇹🇭' },
    { code: 'id', name: 'Indonesian', nativeName: 'Bahasa Indonesia', flag: '🇮🇩' },
    { code: 'ms', name: 'Malay', nativeName: 'Bahasa Melayu', flag: '🇲🇾' },
    { code: 'tl', name: 'Filipino', nativeName: 'Tagalog', flag: '🇵🇭' },
    { code: 'km', name: 'Khmer', nativeName: 'ភាសាខ្មែរ', flag: '🇰🇭' },
    { code: 'hi', name: 'Hindi', nativeName: 'हिन्दी', flag: '🇮🇳' },
    { code: 'bn', name: 'Bengali', nativeName: 'বাংলা', flag: '🇧🇩' },
    // Balkan & Caucasus (6)
    { code: 'hr', name: 'Croatian', nativeName: 'Hrvatski', flag: '🇭🇷' },
    { code: 'sr', name: 'Serbian', nativeName: 'Српски', flag: '🇷🇸' },
    { code: 'sl', name: 'Slovenian', nativeName: 'Slovenščina', flag: '🇸🇮' },
    { code: 'mk', name: 'Macedonian', nativeName: 'Македонски', flag: '🇲🇰' },
    { code: 'ka', name: 'Georgian', nativeName: 'ქართული', flag: '🇬🇪' },
    { code: 'hy', name: 'Armenian', nativeName: 'Հայերեն', flag: '🇦🇲' },
    // Baltic (3)
    { code: 'lt', name: 'Lithuanian', nativeName: 'Lietuvių', flag: '🇱🇹' },
    { code: 'lv', name: 'Latvian', nativeName: 'Latviešu', flag: '🇱🇻' },
    { code: 'et', name: 'Estonian', nativeName: 'Eesti', flag: '🇪🇪' },
    // Celtic & Iberian (4)
    { code: 'cy', name: 'Welsh', nativeName: 'Cymraeg', flag: '🏴󠁧󠁢󠁷󠁬󠁳󠁿' },
    { code: 'gl', name: 'Galician', nativeName: 'Galego', flag: '🇪🇸' },
    { code: 'ca', name: 'Catalan', nativeName: 'Català', flag: '🇪🇸' },
    { code: 'eu', name: 'Basque', nativeName: 'Euskara', flag: '🇪🇸' },
    // African (1)
    { code: 'sw', name: 'Swahili', nativeName: 'Kiswahili', flag: '🇰🇪' },
    // Turkish (1)
    { code: 'tr', name: 'Turkish', nativeName: 'Türkçe', flag: '🇹🇷' },
];

/**
 * Default/fallback language
 */
export const DEFAULT_LANGUAGE: Language = 'en';

/**
 * LocalStorage key for persisting language preference
 */
export const LANGUAGE_STORAGE_KEY = 'aeroftp_language';
