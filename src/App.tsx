import { createBrowserRouter, RouterProvider } from 'react-router-dom';

import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import Accounts from './pages/Accounts';
import Settings from './pages/Settings';
import ApiProxy from './pages/ApiProxy';
import Monitor from './pages/Monitor';
import TokenStats from './pages/TokenStats';
import Security from './pages/Security';
import ThemeManager from './components/common/ThemeManager';
import UserToken from './pages/UserToken';
import { ApiKeyFun } from './pages/ApiKeyFun';
import { UpdateNotification } from './components/UpdateNotification';
import SuggestionDeleteThinkingModal from './components/common/SuggestionDeleteThinkingModal';
import DebugConsole from './components/debug/DebugConsole';
import { useEffect, useState } from 'react';
import { useConfigStore, normalizeLanguageCode } from './stores/useConfigStore';
import { useAccountStore } from './stores/useAccountStore';
import { useViewStore } from './stores/useViewStore';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { isTauri } from './utils/env';
import { request as invoke } from './utils/request';
import { AdminAuthGuard } from './components/common/AdminAuthGuard';

const router = createBrowserRouter([
  {
    path: '/',
    element: <Layout />,
    children: [
      {
        index: true,
        element: <Dashboard />,
      },
      {
        path: 'accounts',
        element: <Accounts />,
      },
      {
        path: 'api-proxy',
        element: <ApiProxy />,
      },
      {
        path: 'monitor',
        element: <Monitor />,
      },
      {
        path: 'token-stats',
        element: <TokenStats />,
      },
      {
        path: 'user-token',
        element: <UserToken />,
      },
      {
        path: 'apikey-fun',
        element: <ApiKeyFun />,
      },
      {
        path: 'security',
        element: <Security />,
      },
      {
        path: 'settings',
        element: <Settings />,
      },
    ],
  },
]);

function App() {
  const { config, loadConfig } = useConfigStore();
  const { fetchCurrentAccount, fetchAccounts } = useAccountStore();
  const { i18n } = useTranslation();

  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

  // Sync language from config (仅在不同步时调度，避免无效重入)
  useEffect(() => {
    if (!config?.language) return;
    const targetLang = normalizeLanguageCode(config.language);
    const currentLang = normalizeLanguageCode(i18n.language);
    if (targetLang !== currentLang) {
      i18n.changeLanguage(targetLang);
      document.documentElement.dir = targetLang === 'ar' ? 'rtl' : 'ltr';
    }
  }, [config?.language]);

  // Listen for tray events
  useEffect(() => {
    if (!isTauri()) return;
    const unlistenPromises: Promise<() => void>[] = [];

    // Listen for the tray's account switch event
    unlistenPromises.push(
      listen('tray://account-switched', () => {
        console.log('[App] Tray account switched, refreshing...');
        fetchCurrentAccount();
        fetchAccounts();
      })
    );

    // Listen for the tray's refresh event
    unlistenPromises.push(
      listen('tray://refresh-current', () => {
        console.log('[App] Tray refresh triggered, refreshing...');
        fetchCurrentAccount();
        fetchAccounts();
      })
    );

    // Tray: leave mini mode and show the full window
    unlistenPromises.push(
      listen('tray://show-window', async () => {
        if (useViewStore.getState().isMiniView) {
          const { exitMiniMode } = await import('./utils/windowManager');
          await exitMiniMode();
          useViewStore.getState().setMiniView(false);
        }
      })
    );

    // Tray: open the floating mini view
    unlistenPromises.push(
      listen('tray://show-mini-view', () => {
        useViewStore.getState().setMiniView(true);
      })
    );

    // Listen for the backend's full-refresh event (Command / Scheduler)
    unlistenPromises.push(
      listen('accounts://refreshed', () => {
        console.log('[App] Backend triggered quota refresh, syncing UI...');
        fetchCurrentAccount();
        fetchAccounts();
      })
    );

    // 监听手动触发自动更新事件
    unlistenPromises.push(
      listen('app://trigger-update', () => {
        console.log('[App] Received app://trigger-update event, showing updater...');
        setShowUpdateNotification(true);
      })
    );

    // Cleanup
    return () => {
      Promise.all(unlistenPromises).then(unlisteners => {
        unlisteners.forEach(unlisten => unlisten());
      });
    };
  }, [fetchCurrentAccount, fetchAccounts]);

  // Update notification state
  const [showUpdateNotification, setShowUpdateNotification] = useState(false);

  // Check for updates on startup
  useEffect(() => {
    const checkUpdates = async () => {
      try {
        console.log('[App] Checking if we should check for updates...');
        const shouldCheck = await invoke<boolean>('should_check_updates');
        console.log('[App] Should check updates:', shouldCheck);

        if (shouldCheck) {
          setShowUpdateNotification(true);
          // We're only responsible for showing the notification component here; internally it calls check_for_updates
          // After showing the component, we mark it as checked (even on failure or no update, the component handles it internally)
          await invoke('update_last_check_time');
          console.log('[App] Update check cycle initiated and last check time updated.');
        }
      } catch (error) {
        console.error('Failed to check update settings:', error);
      }
    };

    // Delay check to avoid blocking initial render
    const timer = setTimeout(checkUpdates, 2000);
    return () => clearTimeout(timer);
  }, []);

  return (
    <AdminAuthGuard>
      <ThemeManager />
      <DebugConsole />
      <SuggestionDeleteThinkingModal />
      {showUpdateNotification && (
        <UpdateNotification onClose={() => setShowUpdateNotification(false)} />
      )}
      <RouterProvider router={router} />
    </AdminAuthGuard>
  );
}

export default App;