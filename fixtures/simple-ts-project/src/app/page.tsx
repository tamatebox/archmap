import { formatPrice } from '@/lib/money';
import type { Money } from '@/lib/types';
import {
  MAX_UPLOAD,
} from '../lib/limits';
import { Button } from '@/components';
import data from './data.json';
import { missing } from '@/lib/missing';
import thing from '~/thing';
import { gone } from './gone';
import pad from 'left-pad';
import type { Handler } from 'aws-lambda';
import { createRoot } from 'react-dom/client';
import { Button as Widget } from 'components/button';

const snippet = `import fake from 'template'`;
const quoted = "import fake from '../admin-test'";
// import fake from "comment";

export default function Page(props: { price: Money }) {
  return <p>Don't pay {formatPrice(props.price)} over {MAX_UPLOAD} {pad} {thing} {missing} {gone} {snippet} {quoted}</p>;
}

export const handler: Handler = async () => ({ data, createRoot, Button, Widget });
