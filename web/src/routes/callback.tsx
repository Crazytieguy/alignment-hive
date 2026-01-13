import { createFileRoute } from '@tanstack/react-router';
import { handleCallbackRoute } from '@workos/authkit-tanstack-react-start';

export const Route = createFileRoute('/callback')({
  server: {
    handlers: {
      GET: async (ctx) => {
        const response = await handleCallbackRoute(ctx);

        // If it's a redirect response, change the Location to /welcome
        if (response.status === 307 || response.status === 302) {
          const headers = new Headers(response.headers);
          headers.set('Location', '/welcome');
          return new Response(response.body, {
            status: response.status,
            headers,
          });
        }

        return response;
      },
    },
  },
});
