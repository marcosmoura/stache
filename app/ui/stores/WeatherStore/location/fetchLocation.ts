import type { LocationData, IpApiResponse, IpInfoResponse } from './types';

const buildDisplayName = (parts: Array<string | undefined>): string =>
  parts.filter(Boolean).join(', ');

const getBrowserLocation = async (): Promise<LocationData | null> => {
  if (!navigator.geolocation) {
    return null;
  }

  return new Promise((resolve) => {
    navigator.geolocation.getCurrentPosition(
      (position) => {
        const { latitude, longitude } = position.coords;
        resolve({
          coordinates: { lat: latitude, lon: longitude },
          displayName: `${latitude},${longitude}`,
        });
      },
      () => resolve(null),
      { timeout: 5000 },
    );
  });
};

const fetchIpApiLocation = async (): Promise<LocationData | null> => {
  try {
    const response = await fetch('https://ipapi.co/json/');

    if (!response.ok) {
      throw new Error('Failed to fetch from ipapi.co');
    }

    const data = (await response.json()) as IpApiResponse;
    const displayName = buildDisplayName([data.city, data.country_name]);

    const location: LocationData = {
      displayName,
    };

    if (data.latitude != null && data.longitude != null) {
      location.coordinates = {
        lat: data.latitude,
        lon: data.longitude,
      };
    }

    return location;
  } catch (error) {
    console.error('ipapi.co error:', error);
    return null;
  }
};

const fetchIpInfoLocation = async (): Promise<LocationData | null> => {
  try {
    const response = await fetch('https://ipinfo.io/json');

    if (!response.ok) {
      throw new Error('Failed to fetch from ipinfo.io');
    }

    const data = (await response.json()) as IpInfoResponse;
    const displayName = buildDisplayName([data.city, data.country]);

    const location: LocationData = {
      displayName,
    };

    if (data.loc) {
      const [lat, lon] = data.loc.split(',').map(Number);
      if (!Number.isNaN(lat) && !Number.isNaN(lon)) {
        location.coordinates = { lat, lon };
      }
    }

    return location;
  } catch (error) {
    console.error('ipinfo.io error:', error);
    return null;
  }
};

export const fetchLocationData = async (defaultLocation: string): Promise<LocationData> => {
  const browserLocation = await getBrowserLocation();

  if (browserLocation) {
    return browserLocation;
  }

  const location = (await fetchIpApiLocation()) ?? (await fetchIpInfoLocation());

  if (location) {
    return location;
  }

  return {
    displayName: defaultLocation,
  };
};
