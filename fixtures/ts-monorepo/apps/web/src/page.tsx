import { Button } from '@acme/ui';
import { VERSION } from '@acme/core';
import { helper } from 'local-lib';
import type { Config } from 'typescript';
import { format } from '@/lib/format';

export const Page = () => [Button, VERSION, helper, format];
