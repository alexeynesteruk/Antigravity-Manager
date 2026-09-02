import { Link, useLocation } from 'react-router-dom';
import { NavigationDropdown } from './NavDropdowns';
import { isActive, getCurrentNavItem, type NavItem } from './constants';
import { useConfigStore } from '../../stores/useConfigStore';

interface NavMenuProps {
    navItems: NavItem[];
}

/**
 * Navigation menu component - handles responsiveness independently
 * 
 * Responsive strategy:
 * - >= 768px (md): text pills
 * - 640px - 768px: icon pills (Logo shows text)
 * - 480px - 640px: icon pills (Logo hides text)
 * - 375px - 480px: icon+text dropdown
 * - < 375px: icon dropdown
 */
export function NavMenu({ navItems }: NavMenuProps) {
    const location = useLocation();
    const { isMenuItemHidden } = useConfigStore();

    // Filter out hidden menu items
    const visibleNavItems = navItems.filter(item => !isMenuItemHidden(item.path));

    return (
        <>
            {/* Text pills (>= 1120px) */}
            <nav className="max-[1119px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map((item) => (
                    <Link
                        key={item.path}
                        to={item.path}
                        draggable="false"
                        className={`
                            px-4 xl:px-6
                            py-2 
                            rounded-full 
                            text-sm 
                            font-medium 
                            transition-all 
                            whitespace-nowrap
                            ${isActive(location.pathname, item.path)
                                ? 'bg-gray-900 text-white shadow-sm dark:bg-white dark:text-gray-900'
                                : 'text-gray-700 hover:text-gray-900 hover:bg-gray-200 dark:text-gray-400 dark:hover:text-base-content dark:hover:bg-base-100'
                            }
                        `}
                    >
                        {item.label}
                    </Link>
                ))}
            </nav>

            {/* Icon pills (880px - 1120px) - Logo shows text */}
            <nav className="max-[879px]:hidden min-[1120px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map((item) => (
                    <Link
                        key={item.path}
                        to={item.path}
                        draggable="false"
                        className={`
                            p-2
                            rounded-full
                            transition-all
                            ${isActive(location.pathname, item.path)
                                ? 'bg-gray-900 text-white shadow-sm dark:bg-white dark:text-gray-900'
                                : 'text-gray-700 hover:text-gray-900 hover:bg-gray-200 dark:text-gray-400 dark:hover:text-base-content dark:hover:bg-base-100'
                            }
                        `}
                        title={item.label}
                    >
                        <item.icon className="w-5 h-5" />
                    </Link>
                ))}
            </nav>

            {/* Icon pills (640px - 880px) - Logo hides text */}
            <nav className="max-[639px]:hidden min-[880px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map((item) => (
                    <Link
                        key={item.path}
                        to={item.path}
                        draggable="false"
                        className={`
                            p-2
                            rounded-full
                            transition-all
                            ${isActive(location.pathname, item.path)
                                ? 'bg-gray-900 text-white shadow-sm dark:bg-white dark:text-gray-900'
                                : 'text-gray-700 hover:text-gray-900 hover:bg-gray-200 dark:text-gray-400 dark:hover:text-base-content dark:hover:bg-base-100'
                            }
                        `}
                        title={item.label}
                    >
                        <item.icon className="w-5 h-5" />
                    </Link>
                ))}
            </nav>

            {/* Icon pills (480px - 640px) */}
            <nav className="max-[479px]:hidden min-[640px]:hidden flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                {visibleNavItems.map((item) => (
                    <Link
                        key={item.path}
                        to={item.path}
                        draggable="false"
                        className={`
                            p-2
                            rounded-full
                            transition-all
                            ${isActive(location.pathname, item.path)
                                ? 'bg-gray-900 text-white shadow-sm dark:bg-white dark:text-gray-900'
                                : 'text-gray-700 hover:text-gray-900 hover:bg-gray-200 dark:text-gray-400 dark:hover:text-base-content dark:hover:bg-base-100'
                            }
                        `}
                        title={item.label}
                    >
                        <item.icon className="w-5 h-5" />
                    </Link>
                ))}
            </nav>

            {/* Icon+text dropdown (375px - 480px) */}
            <div className="max-[374px]:hidden min-[480px]:hidden block">
                <NavigationDropdown
                    navItems={visibleNavItems}
                    isActive={(path) => isActive(location.pathname, path)}
                    getCurrentNavItem={() => getCurrentNavItem(location.pathname, visibleNavItems)}
                    onNavigate={() => { }}
                    showLabel={true}
                />
            </div>

            {/* Icon dropdown (< 375px) */}
            <div className="min-[375px]:hidden">
                <NavigationDropdown
                    navItems={visibleNavItems}
                    isActive={(path) => isActive(location.pathname, path)}
                    getCurrentNavItem={() => getCurrentNavItem(location.pathname, visibleNavItems)}
                    onNavigate={() => { }}
                    showLabel={false}
                />
            </div>
        </>
    );
}
