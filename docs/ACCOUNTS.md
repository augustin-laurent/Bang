# Connexion des comptes dans Bang

## Pour les utilisateurs

Ouvrir **Réglages → Ajouter un compte**, puis choisir un fournisseur.
Aucun identifiant développeur ni fichier de configuration n’est nécessaire.

- **Google** : continuer dans le navigateur système, choisir le compte et
  autoriser Bang. L’adresse est récupérée auprès de Google.
  Dans une préversion sans configuration complète, ce bouton est désactivé.
- **Microsoft** : la connexion est préparée dans le moteur mais reste désactivée
  dans cette version, en attendant l’enregistrement public de l’éditeur.
- **iCloud** : saisir son adresse et un **mot de passe d’application**. Le bouton
  **Créer un mot de passe d’application** ouvre les instructions Apple. Les
  serveurs et les ports sont déjà renseignés ; les modifications éventuelles
  se trouvent sous **Réglages avancés**.
- **Autre compte mail** : saisir son adresse et rechercher les réglages publiés
  par son domaine. Vérifier les serveurs affichés, puis saisir son mot de passe
  ou mot de passe d’application. Si aucun réglage compatible n’est trouvé,
  renseigner les serveurs IMAP/SMTP dans le formulaire avancé.

Bang vérifie l’authentification entrante et sortante avant d’enregistrer le compte.
Cette vérification n’envoie aucun message. La synchronisation démarre ensuite.
Le mot de passe Google/Microsoft est saisi uniquement sur le site du fournisseur.

La croix et Échap annulent la tentative. Une réponse d’une ancienne tentative
ne modifie pas la nouvelle fenêtre de connexion. Les erreurs indiquent s’il faut
réessayer, vérifier les réglages, déverrouiller le trousseau ou reconnecter le compte.

## Stockage sécurisé et sessions

Les nouveaux jetons OAuth et mots de passe sont conservés dans le trousseau Linux
compatible **Secret Service** (par exemple GNOME Keyring ou KWallet). La base
SQLite contient les métadonnées et une référence opaque, pas ces secrets.
Les jetons d’accès sont disponibles dans la mémoire du moteur ; leur renouvellement
est partagé entre les deux workers IMAP et SMTP. Les secrets ne sont jamais copiés
vers les listes de comptes de l’interface ou vers le composeur.

Si le trousseau est verrouillé ou indisponible :

1. Le déverrouiller puis choisir **Réessayer le stockage sécurisé** ; ou
2. Choisir **Continuer pour cette session**. Les identifiants restent en mémoire
   et il faudra se reconnecter au prochain démarrage. Le cache mail reste local.

Aucun repli silencieux vers un fichier de mots de passe en clair n’est effectué.
Si un compte déjà enregistré passe en mode temporaire alors que son ancien
trousseau est inaccessible, les anciennes références restent conservées pour
permettre leur suppression ultérieure ; elles ne servent plus à la connexion.
Le paquet Linux doit permettre l’accès au bus de session D-Bus et au service
`org.freedesktop.secrets`. Une session minimale sans ce service peut utiliser
le mode temporaire. Le trousseau ne protège pas contre une session utilisateur
compromise ou un administrateur système malveillant.

## Expiration, reconnexion et suppression

Bang renouvelle les jetons avant une connexion si leur expiration est proche
(marge de 60 secondes). Les renouvellements simultanés sont regroupés par compte.
Une rotation est enregistrée avant de distribuer les nouveaux jetons ; si la
réponse ne contient pas de nouveau refresh token, l’ancien est conservé.
Une panne réseau garde les identifiants et applique un délai avant réessai.

Un trousseau verrouillé peut être rouvert avec **Déverrouiller** dans les réglages,
sans réinitialiser les jetons en mémoire. **Reconnecter** permet aussi une nouvelle
connexion si le stockage sécurisé reste inaccessible.

Une autorisation révoquée ou un mot de passe refusé entraîne **Reconnexion
nécessaire**, avec l’action **Reconnecter** dans les réglages. Le navigateur
ne s’ouvre jamais spontanément en arrière-plan. Les messages en cache restent
consultables et les envois programmés attendent une autorisation utilisable.

Reconnecter met à jour le compte existant sans effacer son cache. Supprimer un
compte arrête ses workers et ses envois en cours, retire les identifiants du
trousseau et supprime les données locales. Si le trousseau empêche la suppression,
l’opération reste à réessayer. Il s’agit d’un retrait de cet appareil, pas d’une
révocation globale chez le fournisseur ni d’un effacement forensique des sauvegardes.

## Compatibilité avec les comptes existants

Les anciens comptes à mot de passe sont migrés vers le trousseau avant que leurs
champs secrets soient retirés de l’enregistrement SQLite. Un échec de stockage
conserve les données d’origine jusqu’à une action explicite de récupération.
Les anciennes données peuvent subsister dans des pages SQLite/WAL ou sauvegardes ;
aucune promesse d’effacement sécurisé n’est faite.

Les anciens comptes OAuth ne mémorisent pas le client qui a accordé leur jeton.
Ils demandent une reconnexion pour établir cette association correctement, sans
supprimer les messages en cache. Les nouveaux comptes conservent leur enregistrement
émetteur : une mise à jour des identifiants de l’application ne change pas
silencieusement le client utilisé pour renouveler un ancien jeton.

## Pour la personne qui distribue Bang

Le client ID Google **Desktop app** est public. Le paramètre complémentaire
exigé par cet enregistrement doit être fourni par le distributeur à la compilation ;
aucune valeur de ce paramètre n’est enregistrée dans le dépôt. Sans configuration
complète, le bouton Google est désactivé dans les builds de préversion.
Ne jamais embarquer les identifiants confidentiels d’un client Web.

Les valeurs publiques peuvent être fixées à la compilation avec
`BANG_GOOGLE_CLIENT_ID`, `BANG_GOOGLE_DESKTOP_PARAMETER` et, plus tard,
`BANG_MICROSOFT_CLIENT_ID`. Cargo reconstruit les références `option_env!` lorsque
ces variables changent. Le Microsoft client doit être un client public de bureau,
sans secret, autorisant les comptes personnels et d’organisation. La redirection
Google utilise `http://127.0.0.1:<port>/callback` ; Microsoft utilise
`http://localhost:<port>/callback`, enregistré sous Applications mobiles et de bureau.

Le fichier `~/.config/bang/config.toml` peut toujours remplacer un enregistrement
complet pour le développement. Il n’est jamais demandé à un utilisateur final :

```toml
[oauth.google]
client_id = "CLIENT_DE_DEVELOPPEMENT.apps.googleusercontent.com"
# Seulement si nécessaire pour CE client Desktop :
client_secret = "PARAMETRE_PUBLIC_DU_CLIENT_DESKTOP"
```

Un client ID personnalisé n’hérite jamais du paramètre d’un autre enregistrement.
La sauvegarde des préférences ne recopie pas les identifiants embarqués dans ce fichier.

La diffusion publique de Gmail exige une configuration de consentement de production
et la vérification Google correspondant aux scopes `https://mail.google.com/`
(IMAP/SMTP) et `userinfo.email`. Google peut demander une justification du scope
complet ; un client IMAP n’obtient pas une approbation automatique. Le mode test
impose des restrictions et ne remplace pas cette vérification.

Pour Microsoft, les scopes délégués sont `IMAP.AccessAsUser.All`, `SMTP.Send` et
`offline_access`, sur la ressource Exchange. Un jeton Exchange ne sert pas à appeler
Graph. Les règles d’organisation et l’activation d’IMAP/SMTP AUTH restent applicables :
une connexion OAuth ne les contourne pas. Ne pas demander de désactiver les protections
du tenant pour faire fonctionner Bang.

## Découverte des serveurs

Bang consulte uniquement, via HTTPS et sans envoyer l’adresse complète :

- `https://autoconfig.<domaine>/mail/config-v1.1.xml`
- `https://<domaine>/.well-known/autoconfig/mail/config-v1.1.xml`

Chaque demande est limitée à cinq secondes et à 256 Kio. Les redirections, DTD et
entités XML externes sont refusées. Seuls IMAP et SMTP avec TLS ou STARTTLS requis
et une méthode de mot de passe prise en charge sont proposés. Les identifiants
IMAP et SMTP peuvent être différents. Aucun mot de passe n’est envoyé avant la
validation explicite des destinations affichées. Pas de base centrale, de recherche
MX ni de balayage de ports dans cette version.

## Vérification avant distribution

```sh
cargo test -p bang-core
cargo test --workspace --features app/preview-renderer,bang-core/integration
cargo check --workspace
bash scripts/test-keyring.sh
cargo run -p app -- --check-oauth-release
```

Le test de trousseau crée un bus D-Bus et un trousseau temporaires isolés ; il
ne lit pas les identifiants personnels. Les tests OAuth utilisent un serveur simulé.
Avant la publication, essayer le paquet réel avec un profil utilisateur vierge :
consentement Google, synchronisation/envoi, expiration/renouvellement, redémarrage,
annulation du navigateur, révocation, trousseau verrouillé et session temporaire.
Microsoft reste différé tant que son enregistrement public n’est pas fourni.

Références : [OAuth Google Desktop](https://developers.google.com/identity/protocols/oauth2/native-app),
[IMAP/SMTP Gmail](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol),
[OAuth Exchange](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth),
[iCloud](https://support.apple.com/102525), [mots de passe Apple](https://support.apple.com/102654),
[autoconfig](https://github.com/thunderbird/autoconfig/wiki/Autoconfig-file-reference),
[Secret Service](https://specifications.freedesktop.org/secret-service/latest/ch01.html).
