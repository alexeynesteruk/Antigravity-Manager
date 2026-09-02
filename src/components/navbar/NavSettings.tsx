import { Sun, Moon, LogOut, Minimize2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { LanguageDropdown, MoreDropdown } from './NavDropdowns';
import { LANGUAGES } from './constants';
import { isTauri } from '../../utils/env';
import { useViewStore } from '../../stores/useViewStore';

interface NavSettingsProps {
    theme: 'light' | 'dark';
    currentLanguage: string;
    onThemeToggle: (event: React.MouseEvent<HTMLButtonElement>) => void;
    onLanguageChange: (langCode: string) => void;
}

/**
 * Settings button component - handles responsiveness independently
 *
 * Responsive strategy:
 * - >= 768px (md): standalone buttons (theme + language)
 * - < 768px: More dropdown menu
 */
export function NavSettings({
    theme,
    currentLanguage,
    onThemeToggle,
    onLanguageChange
}: NavSettingsProps) {
    const { t } = useTranslation();
    const { setMiniView } = useViewStore();

    const handleLogout = () => {
        sessionStorage.removeItem('abv_admin_api_key');
        localStorage.removeItem('abv_admin_api_key');
        window.location.reload();
    };

    return (
        <>
            {/* Standalone buttons (>= 480px) */}
            <div className="hidden min-[480px]:flex items-center gap-2">
                {/* Mini view toggle button */}
                <button
                    onClick={() => setMiniView(true)}
                    className="w-10 h-10 rounded-full bg-gray-100 dark:bg-base-200 hover:bg-gray-200 dark:hover:bg-base-100 flex items-center justify-center transition-colors"
                    title={t('nav.mini_view', 'Mini View')}
                >
                    <Minimize2 className="w-5 h-5 text-gray-700 dark:text-gray-300" />
                </button>

                {/* Theme toggle button */}
                <button
                    onClick={onThemeToggle}
                    className="w-10 h-10 rounded-full bg-gray-100 dark:bg-base-200 hover:bg-gray-200 dark:hover:bg-base-100 flex items-center justify-center transition-colors"
                    title={theme === 'light' ? t('nav.theme_to_dark') : t('nav.theme_to_light')}
                >
                    {theme === 'light' ? (
                        <Moon className="w-5 h-5 text-gray-700 dark:text-gray-300" />
                    ) : (
                        <Sun className="w-5 h-5 text-gray-700 dark:text-gray-300" />
                    )}
                </button>

                {/* Language switch dropdown menu */}
                <LanguageDropdown
                    currentLanguage={currentLanguage}
                    languages={LANGUAGES}
                    onLanguageChange={onLanguageChange}
                />

                {/* Logout button - shown in Web mode only */}
                {!isTauri() && (
                    <button
                        onClick={handleLogout}
                        className="w-10 h-10 rounded-full bg-red-50 dark:bg-red-900/20 hover:bg-red-100 dark:hover:bg-red-900/40 flex items-center justify-center transition-colors"
                        title={t('nav.logout', 'Logout')}
                    >
                        <LogOut className="w-5 h-5 text-red-600 dark:text-red-400" />
                    </button>
                )}
            </div>

            {/* More menu (< 480px) */}
            <div className="min-[480px]:hidden">
                <MoreDropdown
                    theme={theme}
                    currentLanguage={currentLanguage}
                    languages={LANGUAGES}
                    onThemeToggle={onThemeToggle}
                    onLanguageChange={onLanguageChange}
                />
            </div>
        </>
    );
}
