# Connexion des comptes dans Bang

Bang propose Google, Microsoft, iCloud et un réglage IMAP/SMTP personnalisé.
Les logos Google et Microsoft sont embarqués avec l’application ; aucun service
externe n’est appelé pour afficher les fournisseurs.

## Parcours dans l’application

1. Ouvrir **Réglages → Ajouter un compte**, puis choisir le fournisseur.
2. Google : renseigner éventuellement son adresse pour présélectionner le compte.
   Microsoft : renseigner l’adresse complète de la boîte à connecter.
3. Cliquer sur **Continuer**, autoriser Bang dans le navigateur système, puis
   revenir dans l’application. Pour Microsoft, choisir le compte correspondant
   à l’adresse saisie (ou une boîte partagée à laquelle ce compte a accès).
4. Bang vérifie l’authentification IMAP et SMTP avant d’enregistrer le compte.
   Cette vérification n’envoie aucun mail. La synchronisation commence ensuite.

Pendant la connexion, les boutons sont désactivés pour éviter les doublons.
La croix et Échap annulent la tentative ; un ancien résultat ne ferme pas une
nouvelle fenêtre d’ajout. En cas d’échec, les champs restent disponibles pour
corriger et réessayer. Les détails techniques sont dans les journaux
(`RUST_LOG=bang_core=info,app=info ./target/release/app`).

## Activer Google pour cette application

Cette étape concerne la personne qui prépare Bang, une seule fois pour
l’application. Chaque utilisateur n’a ensuite qu’à autoriser son compte.

1. Dans [Google Cloud Console](https://console.cloud.google.com/), créer ou choisir
   un projet et configurer l’écran de consentement OAuth (Google Auth Platform).
2. En mode test, ajouter l’adresse à utiliser parmi les utilisateurs de test.
3. Créer un client OAuth de type **Application de bureau / Desktop app**.
   Le flux ouvre le navigateur et reçoit son retour sur
   `http://127.0.0.1:<port-libre>/callback`. Ne pas créer un client Web à la place.
4. Copier le client ID et, si Google le fournit/exige pour ce client de bureau,
   son client secret dans `~/.config/bang/config.toml` :

```toml
[oauth.google]
client_id = "VOTRE_CLIENT_ID.apps.googleusercontent.com"
client_secret = "SECRET_DU_CLIENT_DE_BUREAU_SI_NECESSAIRE"
```

Bang relit ces paramètres à chaque tentative : aucun redémarrage nécessaire.
Le client de bureau utilise PKCE et demande `https://mail.google.com/` pour
IMAP/SMTP et `https://www.googleapis.com/auth/userinfo.email` pour l’adresse.
Le client secret d’une application installée ne constitue pas un secret
confidentiel : ne pas utiliser les identifiants d’une application serveur.

Pour distribuer publiquement Bang, suivre la procédure de vérification Google
associée aux autorisations Gmail demandées. Le mode test a aussi des limites
de durée de validité des autorisations.

Références : [OAuth pour applications de bureau](https://developers.google.com/identity/protocols/oauth2/native-app),
[IMAP/SMTP Gmail](https://developers.google.com/workspace/gmail/imap/imap-smtp).

## Activer Microsoft pour cette application

1. Dans le [centre d’administration Microsoft Entra](https://entra.microsoft.com/),
   enregistrer une application. Pour proposer Outlook/Hotmail et Microsoft 365,
   choisir les comptes d’organisations **et** les comptes Microsoft personnels.
2. Dans **Authentification**, ajouter la plateforme **Applications mobiles et de
   bureau**, avec le retour loopback `http://localhost/callback`.
   Respecter exactement le chemin `/callback`. Le port est attribué
   dynamiquement par Bang ; Microsoft ignore ce port pour les redirections
   `localhost` enregistrées. Le serveur écoute uniquement sur `127.0.0.1`.
   Enregistrer cette URI comme client public, pas Web/SPA.
3. Ajouter les autorisations **déléguées Exchange Online**
   `IMAP.AccessAsUser.All` et `SMTP.Send`, ainsi que `offline_access` dans le
   consentement. Un administrateur peut devoir approuver l’application.
4. Copier l’**ID d’application (client)** dans la configuration :

```toml
[oauth.microsoft]
client_id = "VOTRE_ID_APPLICATION"
```

Aucun client secret Microsoft n’est nécessaire pour ce client public PKCE.
Les scopes envoyés incluent leur ressource complète :
`https://outlook.office.com/IMAP.AccessAsUser.All` et
`https://outlook.office.com/SMTP.Send`. L’autorisation passe par le tenant
`common`, compatible avec les comptes personnels et d’organisation.

L’adresse saisie sert d’identité IMAP/SMTP et d’indication au navigateur.
Bang ne tente pas d’appeler Microsoft Graph avec un jeton Exchange.
Vérifier que **IMAP** et **SMTP AUTH** sont autorisés pour la boîte et par les
politiques de l’organisation ; une autorisation OAuth n’active pas ces protocoles.

Références : [OAuth IMAP/SMTP Microsoft](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth),
[redirections loopback](https://learn.microsoft.com/en-us/entra/identity-platform/reply-url).

## iCloud et autres fournisseurs

Pour iCloud, générer un **mot de passe d’application** dans son compte Apple,
puis choisir iCloud dans Bang. Les réglages sont préremplis :

| Protocole | Serveur | Port | Chiffrement |
| --- | --- | --- | --- |
| IMAP | `imap.mail.me.com` | 993 | TLS |
| SMTP | `smtp.mail.me.com` | 587 | STARTTLS |

Utiliser l’adresse complète et le mot de passe d’application. Si nécessaire,
renseigner l’identifiant IMAP indiqué par Apple dans le champ facultatif.
Le moteur emploie actuellement le même identifiant pour IMAP et SMTP.
[Réglages officiels iCloud](https://support.apple.com/102525).

Pour les autres services, choisir **Autre compte mail** et reprendre les
serveurs, ports et identifiants fournis par l’hébergeur. Le chiffrement IMAP et
SMTP se règle séparément ; changer TLS/STARTTLS propose le port habituel,
qui reste modifiable. Laisser l’identifiant vide pour utiliser l’adresse mail.

## Limite actuelle et prochaine étape pour une connexion durable

Le flux initial et la demande de refresh token sont implémentés, mais le moteur
ne renouvelle pas encore automatiquement les jetons OAuth expirés. Une session
IMAP existante peut continuer, puis une reconnexion ou un envoi échouer à
l’expiration. Il faut alors reconnecter le compte avec le même fournisseur.
Cela reste une étape nécessaire avant une distribution à d’autres utilisateurs.

Pour terminer ce comportement :

1. Conserver avec le compte l’ID du client qui a accordé le jeton (et les
   paramètres desktop nécessaires), en gardant les anciens comptes compatibles.
2. Centraliser le renouvellement par compte, avec exclusion des demandes
   simultanées. Avant une connexion IMAP ou un envoi SMTP, renouveler si
   `expires_at` est dans moins de 60 secondes.
3. Appeler le token endpoint du fournisseur avec `grant_type=refresh_token`,
   conserver l’ancien refresh token si la réponse n’en fournit pas de nouveau,
   puis enregistrer atomiquement les jetons et leur échéance.
4. Donner les nouveaux jetons aux deux workers IMAP et au transport SMTP.
   En cas de révocation (`invalid_grant`), afficher **Reconnecter le compte**
   plutôt qu’une boucle de reconnexions automatiques.
5. Couvrir l’expiration, la rotation, deux demandes simultanées, le redémarrage,
   le mode hors ligne et la révocation avec un serveur OAuth simulé.

Les tests de développement n’utilisent aucun compte réel. Le consentement
Google/Microsoft complet reste à essayer avec les clients enregistrés ci-dessus.
