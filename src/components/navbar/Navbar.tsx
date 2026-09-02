import { LayoutDashboard, Users, Network, Activity, BarChart3, Settings, Lock, KeyRound } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useConfigStore } from '../../stores/useConfigStore';
import { isTauri, isLinux } from '../../utils/env';
import { NavLogo } from './NavLogo';
import { NavMenu } from './NavMenu';
import { NavSettings } from './NavSettings';
import type { NavItem } from './constants';

/**
 * Navbar main component
 * 
 * Responsibility: only handles layout and state management, not responsive details
 * Responsive logic is handled independently by each subcomponent
 */
function Navbar() {
    const { t } = useTranslation();
    const { config, saveConfig } = useConfigStore();

    // Create nav items (including translated labels)
    const navItems: NavItem[] = [
        { path: '/', label: t('nav.dashboard'), icon: LayoutDashboard, priority: 'high' },
        { path: '/accounts', label: t('nav.accounts'), icon: Users, priority: 'high' },
        { path: '/api-proxy', label: t('nav.proxy'), icon: Network, priority: 'high' },
        { path: '/apikey-fun', label: t('nav.apikey_fun', 'Relay Station'), icon: KeyRound, priority: 'high' },
        { path: '/monitor', label: t('nav.call_records'), icon: Activity, priority: 'medium' },
        { path: '/token-stats', label: t('nav.token_stats', 'Token Stats'), icon: BarChart3, priority: 'low' },
        { path: '/user-token', label: t('nav.user_token', 'User Tokens'), icon: Users, priority: 'low' },
        { path: '/security', label: t('nav.security'), icon: Lock, priority: 'low' },
        { path: '/settings', label: t('nav.settings'), icon: Settings, priority: 'high' },
    ];

    // Theme toggle logic (with View Transition animation)
    const toggleTheme = async (event: React.MouseEvent<HTMLButtonElement>) => {
        if (!config) return;

        const newTheme = config.theme === 'light' ? 'dark' : 'light';

        // Use View Transition API if supported, but skip on Linux (may cause crash)
        if ('startViewTransition' in document && !isLinux()) {
            const x = event.clientX;
            const y = event.clientY;
            const endRadius = Math.hypot(
                Math.max(x, window.innerWidth - x),
                Math.max(y, window.innerHeight - y)
            );

            // @ts-ignore
            const transition = document.startViewTransition(async () => {
                saveConfig({
                    ...config,
                    theme: newTheme,
                    language: config.language
                }, true);
            });

            transition.ready.then(() => {
                const isDarkMode = newTheme === 'dark';
                const clipPath = isDarkMode
                    ? [`circle(${endRadius}px at ${x}px ${y}px)`, `circle(0px at ${x}px ${y}px)`]
                    : [`circle(0px at ${x}px ${y}px)`, `circle(${endRadius}px at ${x}px ${y}px)`];

                document.documentElement.animate(
                    {
                        clipPath: clipPath
                    },
                    {
                        duration: 500,
                        easing: 'ease-in-out',
                        fill: 'forwards',
                        pseudoElement: isDarkMode ? '::view-transition-old(root)' : '::view-transition-new(root)'
                    }
                );
            });
        } else {
            // Fallback: direct switch (Linux or browsers without View Transition)
            await saveConfig({
                ...config,
                theme: newTheme,
                language: config.language
            }, true);
        }
    };

    // Language switch logic
    const handleLanguageChange = async (langCode: string) => {
        if (!config) return;

        await saveConfig({
            ...config,
            language: langCode,
            theme: config.theme
        }, true);
    };

    return (
        <nav
            style={{ position: 'sticky', top: 0, zIndex: 50 }}
            className="pt-9 transition-all duration-200 bg-[#FAFBFC] dark:bg-base-300"
        >
            {/* Window drag region - Tauri only */}
            {isTauri() && (
                <div
                    className="absolute top-9 left-0 right-0 h-16"
                    style={{ zIndex: 5, backgroundColor: 'rgba(0,0,0,0.001)' }}
                    data-tauri-drag-region
                />
            )}

            <div className="max-w-7xl mx-auto px-8 relative" style={{ zIndex: 10 }}>
                {/* Flexbox layout - subcomponents handle responsiveness themselves */}
                <div className="flex items-center h-16 gap-4">
                    {/* Logo - responsive based on parent container width */}
                    <div className="@container/logo basis-[200px] shrink min-w-0">
                        <NavLogo />
                    </div>

                    {/* Nav menu - handles responsiveness itself */}
                    <div className="flex-1 flex justify-center">
                        <NavMenu navItems={navItems} />
                    </div>

                    {/* Settings button - handles responsiveness itself */}
                    <NavSettings
                        theme={(config?.theme as 'light' | 'dark') || 'light'}
                        currentLanguage={config?.language || 'en'}
                        onThemeToggle={toggleTheme}
                        onLanguageChange={handleLanguageChange}
                    />
                </div>
            </div>
        </nav>
    );
}

export default Navbar;
