import { createContext, use } from 'react';

import { fieldCopy, type FileCopy } from '@/windows/Popover/settingsCopy';

/** The copy of the file whose form is rendered. */
export const CopyContext = createContext<FileCopy>({});

/** The copy of the field at copy path `at`. */
export const useCopy = (at: string) => fieldCopy(use(CopyContext), at);
