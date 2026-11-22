import {
  Activity,
  Blocks,
  CircleHelp,
  CreditCard,
  FolderClock,
  Gauge,
  ScrollText,
  Settings2,
  SlidersHorizontal,
  Sparkles,
  UserRound,
  type LucideIcon,
} from 'lucide-react';

/**
 * The settings directory.
 *
 * The rail used to carry one icon per panel — eight of them, all the same
 * weight and all the same shape, so the rail read as a list of unrelated
 * buttons rather than as navigation. They are all settings surfaces, so they
 * live behind one entry now and this file is the order they appear in.
 *
 * The order is deliberate: what the agent is, then what the app looks like,
 * then the work, then the account, then the reference material. A section that
 * does not fit one of those five groups is a sign the grouping is wrong, not a
 * sign a sixth group is needed.
 */
export type SettingsSectionId =
  | 'config'
  | 'skills'
  | 'plugins'
  | 'context'
  | 'customize'
  | 'sessions'
  | 'billing'
  | 'account'
  | 'status'
  | 'release-notes'
  | 'help';

export interface SettingsSection {
  id: SettingsSectionId;
  label: string;
  /** One line, shown under the label. Says what the section is for, not what it is. */
  hint: string;
  icon: LucideIcon;
}

export interface SettingsGroup {
  label: string;
  sections: SettingsSection[];
}

export const SETTINGS_GROUPS: SettingsGroup[] = [
  {
    label: 'Agent',
    sections: [
      {
        id: 'config',
        label: 'Configuration',
        hint: 'Model, modes and session options',
        icon: Settings2,
      },
      {
        id: 'skills',
        label: 'Skills',
        hint: 'What the agent can load on demand',
        icon: Sparkles,
      },
      {
        id: 'plugins',
        label: 'Plugins',
        hint: 'Installed packages and the panels they ship',
        icon: Blocks,
      },
      {
        id: 'context',
        label: 'Context',
        hint: 'What fills the model window',
        icon: Gauge,
      },
    ],
  },
  {
    label: 'Appearance',
    sections: [
      {
        id: 'customize',
        label: 'Customize',
        hint: 'Theme, layout, actions and panels',
        icon: SlidersHorizontal,
      },
    ],
  },
  {
    label: 'Workspace',
    sections: [
      {
        id: 'sessions',
        label: 'Sessions',
        hint: 'Chat history, archived and deleted',
        icon: FolderClock,
      },
    ],
  },
  {
    label: 'Account',
    sections: [
      {
        id: 'billing',
        label: 'Billing',
        hint: 'Spend and plan limits',
        icon: CreditCard,
      },
      {
        id: 'account',
        label: 'Account',
        hint: 'Identity and plan',
        icon: UserRound,
      },
    ],
  },
  {
    label: 'About',
    sections: [
      {
        id: 'status',
        label: 'Status',
        hint: 'Agent process and connection',
        icon: Activity,
      },
      {
        id: 'release-notes',
        label: 'Release notes',
        hint: 'What changed, version by version',
        icon: ScrollText,
      },
      {
        id: 'help',
        label: 'Help',
        hint: 'Shortcuts and behaviour',
        icon: CircleHelp,
      },
    ],
  },
];

export const SETTINGS_SECTIONS: SettingsSection[] = SETTINGS_GROUPS.flatMap(
  (group) => group.sections,
);

const SECTION_IDS: ReadonlySet<string> = new Set(SETTINGS_SECTIONS.map((s) => s.id));

/**
 * Whether a panel id is one of the settings sections.
 *
 * The rail uses this to keep its single Settings button lit while a section is
 * open, and `setPanel` uses it to route a section id to the settings surface.
 * Both need the same answer, so it is asked in one place.
 */
export function isSettingsSection(id: string | null): id is SettingsSectionId {
  return id !== null && SECTION_IDS.has(id);
}
