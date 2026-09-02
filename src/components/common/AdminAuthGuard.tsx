import React, { useState, useEffect } from 'react';
import { Lock, Key, Globe, AlertCircle, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { isTauri } from '../../utils/env';

/**
 * AdminAuthGuard
 * Mandatory authentication guard layer for Docker/Web mode.
 * If no stored API key is detected or the backend returns 401, the UI is blocked and a key is required.
 */
export const AdminAuthGuard: React.FC<{ children: React.ReactNode }> = ({ children }) => {
    const { t, i18n } = useTranslation();
    const [isAuthenticated, setIsAuthenticated] = useState(isTauri());
    const [apiKey, setApiKey] = useState('');
    const [showLangMenu, setShowLangMenu] = useState(false);
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState('');

    useEffect(() => {
        if (isTauri()) return;

        // Check session storage (priority)
        const sessionKey = sessionStorage.getItem('abv_admin_api_key');
        if (sessionKey) {
            setIsAuthenticated(true);
            setApiKey(sessionKey);
            return;
        }

        // Check local storage (migration logic)
        const savedKey = localStorage.getItem('abv_admin_api_key');
        if (savedKey) {
            // Migrate to sessionStorage and clean up localStorage
            sessionStorage.setItem('abv_admin_api_key', savedKey);
            localStorage.removeItem('abv_admin_api_key');
            setIsAuthenticated(true);
            setApiKey(savedKey);
        }

        // Listen for the global 401 event
        const handleUnauthorized = () => {
            sessionStorage.removeItem('abv_admin_api_key');
            localStorage.removeItem('abv_admin_api_key'); // Double cleanup just in case
            setIsAuthenticated(false);
        };

        window.addEventListener('abv-unauthorized', handleUnauthorized);
        return () => window.removeEventListener('abv-unauthorized', handleUnauthorized);
    }, []);

    const handleLogin = async (e: React.FormEvent) => {
        e.preventDefault();
        const trimmedKey = apiKey.trim();
        if (!trimmedKey) return;

        setIsLoading(true);
        setError('');

        try {
            // Temporarily store the key first, used to validate the request
            sessionStorage.setItem('abv_admin_api_key', trimmedKey);

            // Call an API that requires authentication to verify the password is correct
            const response = await fetch('/api/accounts', {
                method: 'GET',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${trimmedKey}`,
                    'x-api-key': trimmedKey
                }
            });

            if (response.ok || response.status === 204) {
                // Verification succeeded
                localStorage.removeItem('abv_admin_api_key');
                setIsAuthenticated(true);
                window.location.reload();
            } else if (response.status === 401) {
                // Wrong password
                sessionStorage.removeItem('abv_admin_api_key');
                setError(t('login.error_invalid_key'));
            } else {
                // Other error, but the password might be correct
                setIsAuthenticated(true);
                window.location.reload();
            }
        } catch (err) {
            // Network error, etc.
            sessionStorage.removeItem('abv_admin_api_key');
            setError(t('login.error_network'));
        } finally {
            setIsLoading(false);
        }
    };

    const changeLanguage = (lng: string) => {
        i18n.changeLanguage(lng);
        setShowLangMenu(false);
    };

    const languages = [
        { code: 'zh', name: '简体中文' },
        { code: 'zh-TW', name: '繁體中文' },
        { code: 'en', name: 'English' },
        { code: 'ja', name: '日本語' },
        { code: 'ko', name: '한국어' },
        { code: 'ru', name: 'Русский' },
        { code: 'tr', name: 'Türkçe' },
        { code: 'vi', name: 'Tiếng Việt' },
        { code: 'pt', name: 'Português' },
        { code: 'ar', name: 'العربية' },
        { code: 'es', name: 'Español' },
        { code: 'my', name: 'Bahasa Melayu' },
    ];

    if (isAuthenticated) {
        return <>{children}</>;
    }

    return (
        <div className="min-h-screen bg-slate-50 dark:bg-base-300 flex items-center justify-center p-4 relative">
            {/* Language switch button */}
            <div className="absolute top-8 right-8">
                <div className="relative">
                    <button
                        onClick={() => setShowLangMenu(!showLangMenu)}
                        className="flex items-center gap-2 px-4 py-2 bg-white dark:bg-base-100 rounded-2xl shadow-sm border border-slate-100 dark:border-white/5 text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-white/5 transition-all"
                    >
                        <Globe className="w-4 h-4" />
                        <span className="text-sm font-medium uppercase">{i18n.language.split('-')[0]}</span>
                    </button>

                    {showLangMenu && (
                        <div className="absolute right-0 mt-2 w-40 bg-white dark:bg-base-100 rounded-2xl shadow-xl border border-slate-100 dark:border-white/5 py-2 z-50 animate-in fade-in zoom-in duration-200">
                            {languages.map((lang) => (
                                <button
                                    key={lang.code}
                                    onClick={() => changeLanguage(lang.code)}
                                    className={`w-full text-left px-4 py-2 text-sm hover:bg-slate-50 dark:hover:bg-white/5 transition-colors ${i18n.language === lang.code ? 'text-blue-500 font-bold' : 'text-slate-600 dark:text-slate-300'
                                        }`}
                                >
                                    {lang.name}
                                </button>
                            ))}
                        </div>
                    )}
                </div>
            </div>

            <div className="max-w-md w-full bg-white dark:bg-base-100 rounded-3xl shadow-xl overflow-hidden border border-slate-100 dark:border-white/5">
                <div className="p-8">
                    <div className="w-16 h-16 bg-blue-50 dark:bg-blue-900/20 rounded-2xl flex items-center justify-center mb-6 mx-auto">
                        <Lock className="w-8 h-8 text-blue-500" />
                    </div>
                    <h2 className="text-2xl font-bold text-center text-slate-900 dark:text-slate-100 mb-2 font-display">{t('login.title')}</h2>
                    <p className="text-center text-slate-500 dark:text-slate-400 mb-8 text-sm">{t('login.desc')}</p>

                    <form onSubmit={handleLogin} className="space-y-6">
                        <div className="relative">
                            <Key className="absolute left-4 top-1/2 -translate-y-1/2 w-5 h-5 text-slate-400" />
                            <input
                                type="password"
                                placeholder={t('login.placeholder')}
                                className={`w-full pl-12 pr-4 py-4 bg-slate-50 dark:bg-base-200 border-2 rounded-2xl focus:ring-2 focus:ring-blue-500 transition-all outline-none text-slate-900 dark:text-white ${error ? 'border-red-400' : 'border-transparent'}`}
                                value={apiKey}
                                onChange={(e) => { setApiKey(e.target.value); setError(''); }}
                                autoFocus
                                disabled={isLoading}
                            />
                        </div>
                        {error && (
                            <div className="flex items-center gap-2 text-red-500 text-sm">
                                <AlertCircle className="w-4 h-4" />
                                <span>{error}</span>
                            </div>
                        )}
                        <button
                            type="submit"
                            disabled={isLoading || !apiKey.trim()}
                            className="w-full py-4 bg-blue-500 hover:bg-blue-600 disabled:bg-blue-300 disabled:cursor-not-allowed text-white font-bold rounded-2xl shadow-lg shadow-blue-500/30 transition-all active:scale-[0.98] flex items-center justify-center gap-2"
                        >
                            {isLoading ? (
                                <>
                                    <Loader2 className="w-5 h-5 animate-spin" />
                                    {t('login.btn_verifying')}
                                </>
                            ) : (
                                t('login.btn_login')
                            )}
                        </button>
                    </form>

                    <div className="mt-8 pt-6 border-t border-slate-50 dark:border-white/5 text-center">
                        <p className="text-[10px] text-slate-400 leading-relaxed">
                            {t('login.note')}
                            <br />
                            {t('login.lookup_hint')}
                            <br />
                            {t('login.config_hint')}
                        </p>
                    </div>
                </div>
            </div>
        </div>
    );
};
