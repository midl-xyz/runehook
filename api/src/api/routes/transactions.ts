import { TypeBoxTypeProvider } from '@fastify/type-provider-typebox';
import { Type } from '@sinclair/typebox';
import { FastifyPluginCallback } from 'fastify';
import { Server } from 'http';
import { LimitSchema, OffsetSchema, ActivityResponseSchema, TransactionIdSchema, ValidOutputResponseSchema, AddressSchema, VoutSchema } from '../schemas';
import { parseActivityResponse } from '../util/helpers';
import { Optional, PaginatedResponse } from '@hirosystems/api-toolkit';
import { handleCache } from '../util/cache';

export const TransactionRoutes: FastifyPluginCallback<
  Record<never, never>,
  Server,
  TypeBoxTypeProvider
> = (fastify, options, done) => {
  fastify.addHook('preHandler', handleCache);

  fastify.get(
    '/transactions/:tx_id/activity',
    {
      schema: {
        operationId: 'getTransactionActivity',
        summary: 'Transaction activity',
        description: 'Retrieves a paginated list of rune activity for a transaction',
        tags: ['Activity'],
        params: Type.Object({
          tx_id: TransactionIdSchema,
        }),
        querystring: Type.Object({
          offset: Optional(OffsetSchema),
          limit: Optional(LimitSchema),
        }),
        response: {
          200: PaginatedResponse(ActivityResponseSchema, 'Paginated activity response'),
        },
      },
    },
    async (request, reply) => {
      const offset = request.query.offset ?? 0;
      const limit = request.query.limit ?? 20;
      const results = await fastify.db.getTransactionActivity(request.params.tx_id, offset, limit);
      await reply.send({
        limit,
        offset,
        total: results.total,
        results: results.results.map(r => parseActivityResponse(r)),
      });
    }
  );

  fastify.get(
    '/transactions/:tx_id/is-valid-ouptut',
    {
      schema: {
        operationId: 'isValidOutput',
        summary: 'Validates output',
        description: 'Validates the output of the given transaction for certain address',
        tags: ['Output'],
        params: Type.Object({
          tx_id: TransactionIdSchema,
          
        }),
        querystring: Type.Object({
          address: AddressSchema,
          vout: VoutSchema,
        }),
        response: {
          200: ValidOutputResponseSchema,
        },
      },
    },
    async (request, reply) => {
      const results = await fastify.db.outputExists(request.params.tx_id, request.query.address, request.query.vout);
      await reply.send({
        is_valid: results,
      });
    }
  );


  done();
};
