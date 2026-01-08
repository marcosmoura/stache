import { useQuery, useQueryClient } from '@tanstack/react-query';

import { useTauriEvent } from './useTauriEvent';

interface UseTauriEventQueryOptions<TListenPayload, TTransformPayload = TListenPayload> {
  eventName: string;
  transformFn?: (payload: TListenPayload) => TTransformPayload;
  initialFetch?: () => Promise<TListenPayload | null | undefined>;
  queryOptions?: Omit<Parameters<typeof useQuery<TTransformPayload>>[0], 'queryKey' | 'queryFn'>;
}

export function useTauriEventQuery<TListenPayload, TTransformPayload = TListenPayload>({
  eventName,
  transformFn,
  initialFetch,
  queryOptions,
}: UseTauriEventQueryOptions<TListenPayload, TTransformPayload>) {
  const queryClient = useQueryClient();

  useTauriEvent<TListenPayload>(eventName, ({ payload }) => {
    queryClient.setQueryData([eventName], () => {
      return transformFn ? transformFn(payload) : payload;
    });
  });

  const queryFn = initialFetch
    ? async () => {
        const payload = await initialFetch();
        if (payload == null) {
          return undefined as unknown as TTransformPayload;
        }

        return transformFn ? transformFn(payload) : (payload as unknown as TTransformPayload);
      }
    : async () => {
        // Initial data will be set via the event listener
        return undefined as unknown as TTransformPayload;
      };

  return useQuery<TTransformPayload>({
    queryKey: [eventName],
    queryFn,
    ...queryOptions,
    enabled: queryOptions?.enabled ?? Boolean(initialFetch),
  });
}
