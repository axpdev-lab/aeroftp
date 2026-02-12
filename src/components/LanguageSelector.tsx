import * as React from 'react';
import { useState, useRef, useEffect, useMemo } from 'react';
import { ChevronDown, Search, Check, Globe } from 'lucide-react';
import { Language, LanguageInfo, useTranslation } from '../i18n';
import * as Flags from 'country-flag-icons/react/3x2';

// Extract 2-letter country code from flag emoji (regional indicator symbols)
const getFlagCode = (flag: string): string => {
    const codePoints = Array.from(flag).map(c => c.codePointAt(0) || 0);
    if (codePoints.length >= 2 && codePoints[0] >= 0x1F1E6 && codePoints[0] <= 0x1F1FF) {
        return String.fromCharCode(codePoints[0] - 0x1F1E6 + 65, codePoints[1] - 0x1F1E6 + 65);
    }
    return '';
};

// SVG flag icon from country-flag-icons (works on all platforms)
const FlagIcon: React.FC<{ flag: string; size?: 'sm' | 'md' }> = ({ flag, size = 'md' }) => {
    const code = getFlagCode(flag);
    const FlagComponent = code ? (Flags as Record<string, React.FC<React.SVGProps<SVGSVGElement>>>)[code] : null;
    const sizeClasses = size === 'md' ? 'w-9 h-7' : 'w-8 h-6';
    if (FlagComponent) {
        return <FlagComponent className={`${sizeClasses} rounded shadow-sm`} />;
    }
    return <Globe size={size === 'md' ? 24 : 20} className="text-gray-400" />;
};

interface LanguageSelectorProps {
    currentLanguage: Language;
    availableLanguages: LanguageInfo[];
    onSelect: (lang: Language) => void;
    label?: string;
}

// Group languages by region for better organization
const LANGUAGE_REGIONS: Record<string, string[]> = {
    'Popular': ['en', 'es', 'fr', 'de', 'zh', 'ja', 'pt', 'ru', 'it', 'ko'],
    'European': ['nl', 'pl', 'uk', 'ro', 'cs', 'hu', 'el', 'bg', 'sk', 'sv', 'da', 'no', 'fi', 'is'],
    'Asian': ['vi', 'th', 'id', 'ms', 'tl', 'km', 'hi', 'bn'],
    'Middle East': ['tr'],
    'Balkan & Caucasus': ['hr', 'sr', 'sl', 'mk', 'ka', 'hy'],
    'Baltic': ['lt', 'lv', 'et'],
    'Regional': ['cy', 'gl', 'ca', 'eu', 'sw'],
};

export const LanguageSelector: React.FC<LanguageSelectorProps> = ({
    currentLanguage,
    availableLanguages,
    onSelect,
    label
}) => {
    const t = useTranslation();
    const [isOpen, setIsOpen] = useState(false);
    const [searchQuery, setSearchQuery] = useState('');
    const containerRef = useRef<HTMLDivElement>(null);
    const searchInputRef = useRef<HTMLInputElement>(null);

    // Get current language info
    const currentLangInfo = availableLanguages.find(l => l.code === currentLanguage);

    // Filter languages based on search
    const filteredLanguages = useMemo(() => {
        if (!searchQuery.trim()) {
            return availableLanguages;
        }
        const query = searchQuery.toLowerCase();
        return availableLanguages.filter(lang =>
            lang.name.toLowerCase().includes(query) ||
            lang.nativeName.toLowerCase().includes(query) ||
            lang.code.toLowerCase().includes(query)
        );
    }, [availableLanguages, searchQuery]);

    // Translation map for region names
    const getRegionLabel = (regionKey: string): string => {
        const regionMap: Record<string, string> = {
            'Popular': t('ui.language.regionPopular'),
            'European': t('ui.language.regionEuropean'),
            'Asian': t('ui.language.regionAsian'),
            'Middle East': t('ui.language.regionMiddleEast'),
            'Balkan & Caucasus': t('ui.language.regionBalkan'),
            'Baltic': t('ui.language.regionBaltic'),
            'Regional': t('ui.language.regionRegional'),
            'Other': t('ui.language.regionOther'),
            'Search Results': t('ui.language.searchResults'),
        };
        return regionMap[regionKey] || regionKey;
    };

    // Group filtered languages by region
    const groupedLanguages = useMemo(() => {
        if (searchQuery.trim()) {
            // When searching, show flat list
            return { 'Search Results': filteredLanguages };
        }

        const groups: Record<string, LanguageInfo[]> = {};
        const assigned = new Set<string>();

        // Assign languages to regions
        for (const [region, codes] of Object.entries(LANGUAGE_REGIONS)) {
            const langs = filteredLanguages.filter(l => codes.includes(l.code));
            if (langs.length > 0) {
                groups[region] = langs;
                langs.forEach(l => assigned.add(l.code));
            }
        }

        // Add any unassigned languages to "Other"
        const other = filteredLanguages.filter(l => !assigned.has(l.code));
        if (other.length > 0) {
            groups['Other'] = other;
        }

        return groups;
    }, [filteredLanguages, searchQuery]);

    // Close dropdown when clicking outside
    useEffect(() => {
        const handleClickOutside = (e: MouseEvent) => {
            if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
                setIsOpen(false);
                setSearchQuery('');
            }
        };
        document.addEventListener('mousedown', handleClickOutside);
        return () => document.removeEventListener('mousedown', handleClickOutside);
    }, []);

    // Focus search input when opening
    useEffect(() => {
        if (isOpen && searchInputRef.current) {
            searchInputRef.current.focus();
        }
    }, [isOpen]);

    // Handle keyboard navigation
    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'Escape') {
            setIsOpen(false);
            setSearchQuery('');
        }
    };

    const handleSelect = (lang: LanguageInfo) => {
        onSelect(lang.code as Language);
        setIsOpen(false);
        setSearchQuery('');
    };

    return (
        <div ref={containerRef} className="relative" onKeyDown={handleKeyDown}>
            {label && (
                <label className="block text-sm font-medium mb-2 flex items-center gap-2">
                    <Globe size={16} className="text-blue-500" />
                    {label}
                </label>
            )}

            {/* Trigger Button */}
            <button
                type="button"
                onClick={() => setIsOpen(!isOpen)}
                className={`w-full flex items-center gap-3 px-4 py-3 rounded-lg border-2 transition-all bg-white dark:bg-gray-800 ${
                    isOpen
                        ? 'border-blue-500 ring-2 ring-blue-500/20'
                        : 'border-gray-200 dark:border-gray-600 hover:border-gray-300 dark:hover:border-gray-500'
                }`}
            >
                {currentLangInfo ? <FlagIcon flag={currentLangInfo.flag} size="md" /> : <Globe size={24} className="text-gray-400" />}
                <div className="flex-1 text-left">
                    <p className="font-medium">{currentLangInfo?.nativeName || t('ui.language.selectLanguage')}</p>
                    <p className="text-xs text-gray-500">{currentLangInfo?.name}</p>
                </div>
                <ChevronDown
                    size={20}
                    className={`text-gray-400 transition-transform ${isOpen ? 'rotate-180' : ''}`}
                />
            </button>

            {/* Dropdown */}
            {isOpen && (
                <div className="absolute z-50 mt-2 w-full bg-white dark:bg-gray-800 rounded-xl shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in">
                    {/* Search Input */}
                    <div className="p-3 border-b border-gray-200 dark:border-gray-700">
                        <div className="relative">
                            <Search size={16} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                            <input
                                ref={searchInputRef}
                                type="text"
                                value={searchQuery}
                                onChange={(e) => setSearchQuery(e.target.value)}
                                placeholder={t('ui.language.searchPlaceholder')}
                                className="w-full pl-9 pr-4 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/50"
                            />
                        </div>
                        <p className="text-xs text-gray-400 mt-2 text-center">
                            {t('ui.language.languagesAvailable', { count: availableLanguages.length })}
                        </p>
                    </div>

                    {/* Language List */}
                    <div className="max-h-80 overflow-y-auto">
                        {Object.entries(groupedLanguages).map(([region, langs]) => (
                            <div key={region}>
                                {/* Region Header */}
                                {!searchQuery.trim() && (
                                    <div className="px-4 py-2 bg-gray-50 dark:bg-gray-900/50 text-xs font-semibold text-gray-500 uppercase tracking-wide sticky top-0">
                                        {getRegionLabel(region)}
                                    </div>
                                )}

                                {/* Languages in Region */}
                                {langs.map(lang => (
                                    <button
                                        key={lang.code}
                                        onClick={() => handleSelect(lang)}
                                        className={`w-full flex items-center gap-3 px-4 py-2.5 hover:bg-gray-50 dark:hover:bg-gray-700/50 transition-colors ${
                                            currentLanguage === lang.code
                                                ? 'bg-blue-50 dark:bg-blue-900/30'
                                                : ''
                                        }`}
                                    >
                                        <FlagIcon flag={lang.flag} size="sm" />
                                        <div className="flex-1 text-left min-w-0">
                                            <p className={`font-medium truncate ${
                                                currentLanguage === lang.code
                                                    ? 'text-blue-600 dark:text-blue-400'
                                                    : ''
                                            }`}>
                                                {lang.nativeName}
                                            </p>
                                            <p className="text-xs text-gray-500 truncate">{lang.name}</p>
                                        </div>
                                        {currentLanguage === lang.code && (
                                            <Check size={18} className="text-blue-500 flex-shrink-0" />
                                        )}
                                    </button>
                                ))}
                            </div>
                        ))}

                        {filteredLanguages.length === 0 && (
                            <div className="p-8 text-center text-gray-500">
                                <Globe size={32} className="mx-auto mb-2 opacity-30" />
                                <p>{t('ui.language.noLanguages')}</p>
                                <p className="text-sm">{t('ui.language.tryDifferentSearch')}</p>
                            </div>
                        )}
                    </div>
                </div>
            )}
        </div>
    );
};

export default LanguageSelector;
