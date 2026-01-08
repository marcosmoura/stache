import { Stack } from '@/components/Stack';

import { Battery } from './Battery';
import { Clock } from './Clock';
import { Cpu } from './Cpu';
import { KeepAwake } from './KeepAwake';
import { Weather } from './Weather';

export const Status = () => {
  return (
    <Stack data-test-id="status-container">
      <Weather />
      <KeepAwake />
      <Cpu />
      <Battery />
      <Clock />
    </Stack>
  );
};
