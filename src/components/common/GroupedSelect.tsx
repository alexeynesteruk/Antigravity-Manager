import { useState, useRef, useEffect } from 'react';
import { createPortal } from 'react-dom';
import { ChevronDown, Check, Edit3 } from 'lucide-react';
import { cn } from '../../utils/cn';

export interface SelectOption {
    value: string;
    label: string;
    group?: string;
}

interface GroupedSelectProps {
    value: string;
    onChange: (value: string) => void;
    options: SelectOption[];
    placeholder?: string;
    className?: string;
    disabled?: boolean;
    allowCustomInput?: boolean; // Added: whether to allow custom input
}

export default function GroupedSelect({
    value,
    onChange,
    options,
    placeholder = 'Select...',
    className = '',
    disabled = false,
    allowCustomInput = false // Added: custom input is not allowed by default
}: GroupedSelectProps) {
    const [isOpen, setIsOpen] = useState(false);
    const [dropdownPosition, setDropdownPosition] = useState({ top: 0, left: 0, width: 0 });
    const [customInput, setCustomInput] = useState(''); // Added: custom input value
    const containerRef = useRef<HTMLDivElement>(null);
    const buttonRef = useRef<HTMLButtonElement>(null);
    const dropdownRef = useRef<HTMLDivElement>(null); // Added: dropdown menu ref
    const customInputRef = useRef<HTMLInputElement>(null); // Added: custom input field ref

    // Group options by group
    const groupedOptions = options.reduce((acc, option) => {
        const group = option.group || 'Other';
        if (!acc[group]) {
            acc[group] = [];
        }
        acc[group].push(option);
        return acc;
    }, {} as Record<string, SelectOption[]>);

    // Get the label of the currently selected item
    const selectedOption = options.find(opt => opt.value === value);
    const selectedLabel = selectedOption?.label || value || placeholder;

    // Update the dropdown menu position
    const updateDropdownPosition = () => {
        if (buttonRef.current) {
            const rect = buttonRef.current.getBoundingClientRect();
            setDropdownPosition({
                top: rect.bottom + window.scrollY + 4,
                left: rect.left + window.scrollX,
                width: Math.max(rect.width * 1.1, 220) // Widen to 1.1x, minimum 220px
            });
        }
    };

    // Close the dropdown when clicking outside
    useEffect(() => {
        const handleClickOutside = (event: MouseEvent) => {
            // Fix: check whether the click is inside the container or the dropdown menu
            const target = event.target as Node;
            const isClickInsideContainer = containerRef.current?.contains(target);
            const isClickInsideDropdown = dropdownRef.current?.contains(target);

            if (!isClickInsideContainer && !isClickInsideDropdown) {
                setIsOpen(false);
            }
        };

        if (isOpen) {
            updateDropdownPosition();
            document.addEventListener('mousedown', handleClickOutside);
            window.addEventListener('scroll', updateDropdownPosition, true);
            window.addEventListener('resize', updateDropdownPosition);
        }

        return () => {
            document.removeEventListener('mousedown', handleClickOutside);
            window.removeEventListener('scroll', updateDropdownPosition, true);
            window.removeEventListener('resize', updateDropdownPosition);
        };
    }, [isOpen]);

    const handleSelect = (optionValue: string) => {
        console.log('[GroupedSelect] handleSelect called:', optionValue);
        onChange(optionValue);
        setIsOpen(false);
    };

    const handleCustomInputSubmit = () => {
        if (customInput.trim()) {
            console.log('[GroupedSelect] Custom input submitted:', customInput.trim());
            onChange(customInput.trim());
            setCustomInput('');
            setIsOpen(false);
        }
    };

    const handleToggle = () => {
        if (!disabled) {
            setIsOpen(!isOpen);
            if (!isOpen) {
                updateDropdownPosition();
            }
        }
    };

    return (
        <div ref={containerRef} className={cn('relative', className)}>
            {/* Trigger button */}
            <button
                ref={buttonRef}
                type="button"
                onClick={handleToggle}
                disabled={disabled}
                className={cn(
                    'w-full px-3 py-2 text-left text-xs font-mono',
                    'bg-white dark:bg-gray-800',
                    'border border-gray-300 dark:border-gray-600',
                    'rounded-lg',
                    'flex items-center justify-between gap-2',
                    'transition-all duration-200',
                    'hover:border-blue-400 dark:hover:border-blue-500',
                    'focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent',
                    disabled && 'opacity-50 cursor-not-allowed',
                    isOpen && 'ring-2 ring-blue-500 border-transparent'
                )}
            >
                <span className="truncate text-gray-900 dark:text-gray-100">
                    {selectedLabel}
                </span>
                <ChevronDown
                    size={14}
                    className={cn(
                        'text-gray-500 dark:text-gray-400 transition-transform duration-200',
                        isOpen && 'rotate-180'
                    )}
                />
            </button>

            {/* Dropdown menu - rendered to body via Portal */}
            {isOpen && createPortal(
                <div
                    ref={dropdownRef}
                    style={{
                        position: 'absolute',
                        top: `${dropdownPosition.top}px`,
                        left: `${dropdownPosition.left}px`,
                        width: `${dropdownPosition.width}px`,
                        zIndex: 9999
                    }}
                    className={cn(
                        'bg-white dark:bg-gray-800',
                        'border border-gray-200 dark:border-gray-700',
                        'rounded-lg shadow-2xl',
                        'max-h-80 overflow-y-auto',
                        'animate-in fade-in-0 zoom-in-95 duration-100'
                    )}
                >
                    {Object.entries(groupedOptions).map(([group, groupOptions]) => (
                        <div key={group}>
                            {/* Group title */}
                            <div className="px-3 py-1.5 text-[9px] font-bold text-gray-500 dark:text-gray-400 uppercase tracking-wider bg-gray-50 dark:bg-gray-900/50 sticky top-0 z-10">
                                {group}
                            </div>

                            {/* Group options */}
                            {groupOptions.map((option) => (
                                <button
                                    key={option.value}
                                    type="button"
                                    onClick={() => handleSelect(option.value)}
                                    title={option.label}
                                    className={cn(
                                        'w-full px-3 py-1.5 text-left text-[10px] font-mono',
                                        'flex items-center justify-between gap-2',
                                        'transition-colors duration-150',
                                        'hover:bg-blue-50 dark:hover:bg-blue-900/20',
                                        option.value === value
                                            ? 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
                                            : 'text-gray-900 dark:text-gray-100'
                                    )}
                                >
                                    <span className="truncate">{option.label}</span>
                                    {option.value === value && (
                                        <Check size={12} className="text-blue-600 dark:text-blue-400 flex-shrink-0" />
                                    )}
                                </button>
                            ))}
                        </div>
                    ))}

                    {/* Custom input area */}
                    {allowCustomInput && (
                        <div className="border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/50 p-2">
                            <div className="flex items-center gap-1.5">
                                <Edit3 size={12} className="text-gray-400 dark:text-gray-500 flex-shrink-0" />
                                <input
                                    ref={customInputRef}
                                    type="text"
                                    value={customInput}
                                    onChange={(e) => setCustomInput(e.target.value)}
                                    onKeyDown={(e) => {
                                        if (e.key === 'Enter') {
                                            e.preventDefault();
                                            handleCustomInputSubmit();
                                        }
                                    }}
                                    placeholder="Enter a custom model ID..."
                                    className={cn(
                                        'flex-1 px-2 py-1 text-[10px] font-mono',
                                        'bg-white dark:bg-gray-800',
                                        'border border-gray-300 dark:border-gray-600',
                                        'rounded focus:outline-none focus:ring-1 focus:ring-blue-500',
                                        'text-gray-900 dark:text-gray-100',
                                        'placeholder:text-gray-400 dark:placeholder:text-gray-500'
                                    )}
                                />
                                <button
                                    type="button"
                                    onClick={handleCustomInputSubmit}
                                    disabled={!customInput.trim()}
                                    className={cn(
                                        'px-2 py-1 text-[10px] font-medium rounded',
                                        'transition-colors duration-150',
                                        customInput.trim()
                                            ? 'bg-blue-500 hover:bg-blue-600 text-white'
                                            : 'bg-gray-200 dark:bg-gray-700 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                                    )}
                                >
                                    Confirm
                                </button>
                            </div>
                        </div>
                    )}
                </div>,
                document.body
            )}
        </div>
    );
}
