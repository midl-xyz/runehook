import { TypeBoxTypeProvider } from '@fastify/type-provider-typebox';
import { Type } from '@sinclair/typebox';
import { FastifyPluginCallback } from 'fastify';
import { Server } from 'http';
import { Value } from '@sinclair/typebox/value';
import {
  LimitSchema,
  OffsetSchema,
  ActivityResponseSchema,
  TransactionIdSchema,
  AmountOutputResponseSchema,
  AddressSchema,
  VoutSchema,
  RuneSchema,
  NotFoundResponse,
} from '../schemas';
import { parseActivityResponse, parseAmountResponse } from '../util/helpers';
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
    '/transactions/:tx_id/valid-ouptut',
    {
      schema: {
        operationId: 'isValidOutput',
        summary: 'Validates output',
        description:
          'Validates the output of the given transaction for certain address, returning UTXO if the output is valid.',
        tags: ['Output'],
        params: Type.Object({
          tx_id: TransactionIdSchema,
        }),
        querystring: Type.Object({
          address: AddressSchema,
          vout: VoutSchema,
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
      const results = await fastify.db.getUtxoOfOutput(
        request.params.tx_id,
        request.query.address,
        request.query.vout
      );
      await reply.send({
        limit: limit,
        offset: offset,
        total: results.length,
        results: results.map(r => parseActivityResponse(r)),
      });
    }
  );

  fastify.get(
    '/transactions/:tx_id/amount/:rune_id',
    {
      schema: {
        operationId: 'getTxVoutRuneAmount',
        summary: 'Get the amount of the specified rune in the transaction.',
        tags: ['Output'],
        params: Type.Object({
          tx_id: TransactionIdSchema,
          rune_id: RuneSchema,
        }),
        querystring: Type.Object({
          vout: VoutSchema,
        }),
        response: {
          404: NotFoundResponse,
          200: AmountOutputResponseSchema,
        },
      },
    },
    async (request, reply) => {
      const results = await fastify.db.getTxIdOutputRuneAmount(
        request.params.tx_id,
        request.query.vout,
        request.params.rune_id
      );
      if (results.length == 0) {
        await reply.code(404).send(Value.Create(NotFoundResponse));
      } else {
        await reply.send(parseAmountResponse(request.params.rune_id, results));
      }
    }
  );

  done();
};
