import { money, formatPrice } from '.';
import * as all from '.';
import { Badge } from './badge';
import * as ui from './badge';

export function View() {
  return (
    <div>
      <Badge label={money.formatPrice(1)} />
      <Badge label={formatPrice(2)} />
      <ui.Badge label={all.money.formatPrice(3)}></ui.Badge>
    </div>
  );
}
