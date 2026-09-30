from google.cloud import bigquery
import sklearn.metrics
import google.api_core.exceptions


def _client():
    return bigquery.Client(), sklearn.metrics, google.api_core.exceptions
